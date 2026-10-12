"""The v1 catalog contract of the PostgreSQL reference backend (CE-04-01; Owner G-089..G-098;
`spec/persistence.md`, "v1 catalog contract").

Open compares an existing schema with the finite catalog state the v1 initializer produces, apart
from catalog identity that cannot change semantics: object names other than the four table names and
the column names, OIDs, ownership and grants, column order, dropped-column residue, planner
statistics, comments and the closed set of engine bookkeeping fields (G-096). Engine-generated
fields outside that set are compared (G-098): type copies for consistency with their type, relation
counts as invariants, the rest as the initializer leaves them. The internal members of the v1 island
(TOAST relations and indexes, their attributes, row and array types) are compared over every column
of their catalog rows except the named exclusions (`MEMBER_EXCLUSIONS`). No object outside the island
may record a dependency on it, and no island object on anything but the namespace and built-ins
(G-094, G-095); no security label may sit on it (G-097). Comparison is split into `fetch` (queries)
and `mismatch` (a pure function over the fetched rows), so that properties PostgreSQL will not let an
ordinary role construct are still checked by direct predicate tests.

Every property is read as structured catalog data, never as SQL that PostgreSQL renders (G-091).
Expressions are compared as their stored node trees, with column references mapped to column names
and source locations removed. Every catalog relation, type, function and operator in these queries
is named through `pg_catalog`: the temporary schema is searched first for relations and types even
when the search path does not name it (CE04-CP-001), and user objects in any schema must never stand
in for built-ins (WP04-CR-007).
"""

from __future__ import annotations

import re
from dataclasses import dataclass, fields
from typing import Any

import psycopg

SERVER_MAJOR = 14
FIRST_NORMAL_OBJECT_ID = 16384  # PostgreSQL's first OID for objects not created by initdb (built-ins)
DATABASE_ENCODING = "UTF8"
DATABASE_LOCALES = frozenset({"C", "POSIX"})

Rows = tuple[tuple[Any, ...], ...]


@dataclass(frozen=True)
class Catalog:
    """The catalog state of the `mneme_` relations of one namespace, OID-free and order-free."""

    relations: Rows  # every non-index relation whose name begins `mneme_`
    inheritance: Rows
    policies: Rows
    rules: Rows
    triggers: Rows
    columns: Rows
    defaults: Rows
    constraints: Rows  # on those relations, or referencing them from anywhere
    indexes: Rows
    index_columns: Rows  # the attributes of their indexes, whole rows (G-098)
    members: Rows  # internal island members, whole rows (CE04-CP-005)
    boundary: Rows  # dependency edges crossing the island, and security labels on it


def environment_mismatch(server_version_num: int, encoding: str, collate: str, ctype: str) -> str | None:
    """G-090/G-091: PostgreSQL major 14, a UTF8 database, and C (or POSIX) collation and ctype."""
    if server_version_num // 10000 != SERVER_MAJOR:
        return f"server version {server_version_num} is not PostgreSQL {SERVER_MAJOR}"
    if encoding != DATABASE_ENCODING:
        return f"database encoding {encoding} is not {DATABASE_ENCODING}"
    if collate not in DATABASE_LOCALES or ctype not in DATABASE_LOCALES:
        return f"database LC_COLLATE {collate!r} / LC_CTYPE {ctype!r} is not C or POSIX"
    return None


def mismatch(found: Catalog, expected: Catalog) -> str | None:
    """The first catalog part in which `found` differs from `expected`, or None if it is v1."""
    for part in fields(Catalog):
        if getattr(found, part.name) != getattr(expected, part.name):
            return part.name
    return None


# ----------------------------------------------------------------------------------------- fetch

ENVIRONMENT_SQL = (
    "SELECT pg_catalog.current_setting('server_version_num'),"
    " pg_catalog.pg_encoding_to_char(d.encoding)::pg_catalog.text,"
    " d.datcollate::pg_catalog.text, d.datctype::pg_catalog.text"
    " FROM pg_catalog.pg_database d"
    " WHERE d.datname OPERATOR(pg_catalog.=) pg_catalog.current_database()"
)
NAMESPACE_SQL = (
    "SELECT n.oid::pg_catalog.text, n.nspname::pg_catalog.text FROM pg_catalog.pg_namespace n"
    " WHERE n.nspname OPERATOR(pg_catalog.=) pg_catalog.current_schema()"
)

# The `mneme_` non-index relations of the namespace (`%(ns)s` is its OID as text).
_SCOPE = (
    "WITH t AS (SELECT c.oid, c.relname::pg_catalog.text AS name FROM pg_catalog.pg_class c"
    " WHERE c.relnamespace OPERATOR(pg_catalog.=) %(ns)s::pg_catalog.oid"
    " AND c.relkind OPERATOR(pg_catalog.<>) 'i' AND c.relkind OPERATOR(pg_catalog.<>) 'I'"
    " AND pg_catalog.starts_with(c.relname::pg_catalog.text, 'mneme_')) "
)
_IN_SCOPE = "OPERATOR(pg_catalog.=) ANY (SELECT t.oid FROM t)"
_EQ = "OPERATOR(pg_catalog.=)"
_NE = "OPERATOR(pg_catalog.<>)"


def _names(array: str, relation: str) -> str:
    """The attribute names of an attribute-number array of `relation`, in array order."""
    return (
        f"ARRAY(SELECT a.attname::pg_catalog.text FROM pg_catalog.unnest({array}) WITH ORDINALITY"
        f" AS u(k, n) LEFT JOIN pg_catalog.pg_attribute a ON a.attrelid {_EQ} {relation}"
        f" AND a.attnum {_EQ} u.k ORDER BY u.n)"
    )


def _qualified(array: str, catalog: str, name: str, namespace: str) -> str:
    """`schema.name` of each OID in an OID array, in array order ('' for OID 0)."""
    return (
        f"ARRAY(SELECT pg_catalog.concat(n.nspname, '.', o.{name}) FROM pg_catalog.unnest({array})"
        f" WITH ORDINALITY AS u(k, i) LEFT JOIN pg_catalog.{catalog} o ON o.oid {_EQ} u.k"
        f" LEFT JOIN pg_catalog.pg_namespace n ON n.oid {_EQ} o.{namespace} ORDER BY u.i)"
    )


def _catalog(name: str) -> str:
    """The OID of the system catalog `pg_catalog.<name>`."""
    return f"'pg_catalog.{name}'::pg_catalog.regclass::pg_catalog.oid"


# The island's internal members (`tx` TOAST relations, `txi` their indexes, `ty` row and array types,
# each with the backlinks a certified member must have), and the whole island.
_ISLAND = (
    ", tx AS (SELECT t.name, c.reltoastrelid AS oid FROM t"
    f" JOIN pg_catalog.pg_class c ON c.oid {_EQ} t.oid WHERE c.reltoastrelid {_NE} 0)"
    ", txi AS (SELECT tx.name, i.indexrelid AS oid FROM tx"
    f" JOIN pg_catalog.pg_index i ON i.indrelid {_EQ} tx.oid)"
    ", ty AS (SELECT t.name, 'row' AS kind, c.reltype AS oid, t.oid AS relid, 0::pg_catalog.oid AS elem,"
    " y.typarray AS arr FROM t"
    f" JOIN pg_catalog.pg_class c ON c.oid {_EQ} t.oid JOIN pg_catalog.pg_type y ON y.oid {_EQ} c.reltype"
    " UNION ALL SELECT t.name, 'array', y.typarray, 0::pg_catalog.oid, c.reltype, 0::pg_catalog.oid FROM t"
    f" JOIN pg_catalog.pg_class c ON c.oid {_EQ} t.oid JOIN pg_catalog.pg_type y ON y.oid {_EQ} c.reltype"
    f" WHERE y.typarray {_NE} 0)"
    f", island(classid, objid) AS (SELECT {_catalog('pg_class')}, t.oid FROM t"
    f" UNION ALL SELECT {_catalog('pg_class')}, i.indexrelid FROM pg_catalog.pg_index i"
    f" WHERE i.indrelid {_IN_SCOPE}"
    f" UNION ALL SELECT {_catalog('pg_class')}, tx.oid FROM tx"
    f" UNION ALL SELECT {_catalog('pg_class')}, txi.oid FROM txi"
    f" UNION ALL SELECT {_catalog('pg_type')}, ty.oid FROM ty"
    f" UNION ALL SELECT {_catalog('pg_constraint')}, k.oid FROM pg_catalog.pg_constraint k"
    f" WHERE k.conrelid {_IN_SCOPE}"
    f" UNION ALL SELECT {_catalog('pg_attrdef')}, d.oid FROM pg_catalog.pg_attrdef d"
    f" WHERE d.adrelid {_IN_SCOPE}"
    f" UNION ALL SELECT {_catalog('pg_trigger')}, g.oid FROM pg_catalog.pg_trigger g"
    f" WHERE g.tgrelid {_IN_SCOPE}) "
)


def _member(classid: str, objid: str) -> str:
    return f"EXISTS (SELECT 1 FROM island s WHERE s.classid {_EQ} {classid} AND s.objid {_EQ} {objid})"


def _name_of(oid: str, catalog: str, name: str, namespace: str) -> str:
    """`schema.name` of one object (NULL for OID 0)."""
    return (
        f"(SELECT pg_catalog.concat(n.nspname, '.', o.{name}) FROM pg_catalog.{catalog} o"
        f" JOIN pg_catalog.pg_namespace n ON n.oid {_EQ} o.{namespace} WHERE o.oid {_EQ} {oid})"
    )


def _whole(alias: str, excluded: frozenset[str], normalized: dict[str, str]) -> str:
    """The row `alias` as jsonb: every column except `excluded`, OID associations as `normalized`."""
    removed = "{" + ",".join(sorted(excluded | set(normalized))) + "}"
    pairs = ", ".join(f"'{key}', {expression}" for key, expression in sorted(normalized.items()))
    return (
        f"(pg_catalog.to_jsonb({alias}) OPERATOR(pg_catalog.-) '{removed}'::pg_catalog.text[]"
        f" OPERATOR(pg_catalog.||) pg_catalog.jsonb_build_object({pairs}))"
    )


MEMBER_EXCLUSIONS: dict[str, frozenset[str]] = {
    # The closed exclusion list of classification rev. 5 §10.1: identity (G-089), names (G-089),
    # ownership and grants (G-089), planner statistics (G-089), engine bookkeeping (G-096).
    "pg_class": frozenset(
        {
            "oid",
            "relname",
            "relfilenode",
            "relowner",
            "relacl",
            "relpages",
            "reltuples",
            "relallvisible",
            "relhasindex",
            "relhasrules",
            "relhastriggers",
            "relhassubclass",
            "relfrozenxid",
            "relminmxid",
        }
    ),
    "pg_index": frozenset({"indexrelid", "indrelid", "indcheckxmin"}),
    "pg_attribute": frozenset({"attrelid", "attname", "attacl", "attstattarget", "attoptions"}),
    "pg_type": frozenset({"oid", "typname", "typowner", "typacl"}),
}
"""Catalog columns a whole-row member comparison leaves out, each for a named reason."""


# Columns compared through a normalization: associations by name or backlink, type copies by
# consistency, counts as invariants (G-098). Every other column is compared as stored.
def _relnatts(alias: str) -> str:
    """G-098 invariant: `relnatts` is the highest attribute number (dropped residue included)."""
    return (
        f"{alias}.relnatts {_EQ} (SELECT COALESCE(pg_catalog.max(x.attnum), 0)"
        f" FROM pg_catalog.pg_attribute x WHERE x.attrelid {_EQ} {alias}.oid"
        " AND x.attnum OPERATOR(pg_catalog.>) 0)"
    )


def _relchecks(alias: str) -> str:
    """G-098 invariant: `relchecks` is the number of the relation's actual CHECK constraints."""
    return (
        f"{alias}.relchecks::pg_catalog.int8 {_EQ} (SELECT pg_catalog.count(*)"
        " FROM pg_catalog.pg_constraint k"
        f" WHERE k.conrelid {_EQ} {alias}.oid AND k.contype {_EQ} 'c')"
    )


_CLASS_NORMALIZED = {
    "relnamespace": "(SELECT n.nspname::pg_catalog.text FROM pg_catalog.pg_namespace n"
    f" WHERE n.oid {_EQ} c.relnamespace)",
    "relam": f"(SELECT am.amname::pg_catalog.text FROM pg_catalog.pg_am am WHERE am.oid {_EQ} c.relam)",
    "reltype": f"c.reltype {_NE} 0",
    "relnatts": _relnatts("c"),
    "relchecks": _relchecks("c"),
}


def _copy_of(column: str, type_column: str) -> str:
    """G-098: an attribute's copy of its type's representation, as consistency with that type."""
    return f"(SELECT a.{column} {_EQ} y.{type_column} FROM pg_catalog.pg_type y WHERE y.oid {_EQ} a.atttypid)"


_ATTRIBUTE_NORMALIZED = {
    "atttypid": _name_of("a.atttypid", "pg_type", "typname", "typnamespace"),
    "attcollation": _name_of("a.attcollation", "pg_collation", "collname", "collnamespace"),
    "attlen": _copy_of("attlen", "typlen"),
    "attbyval": _copy_of("attbyval", "typbyval"),
    "attalign": _copy_of("attalign", "typalign"),
}
_TYPE_FUNCTIONS = (
    "typinput",
    "typoutput",
    "typreceive",
    "typsend",
    "typmodin",
    "typmodout",
    "typanalyze",
    "typsubscript",
)
_TYPE_NORMALIZED = {
    "typnamespace": f"y.typnamespace {_EQ} %(ns)s::pg_catalog.oid",
    "typrelid": f"y.typrelid {_EQ} ty.relid",
    "typelem": f"y.typelem {_EQ} ty.elem",
    "typarray": f"y.typarray {_EQ} ty.arr",
    **{f: _name_of(f"y.{f}::pg_catalog.oid", "pg_proc", "proname", "pronamespace") for f in _TYPE_FUNCTIONS},
}
_INDEX_NORMALIZED = {
    "indclass": _qualified("i.indclass::pg_catalog.oid[]", "pg_opclass", "opcname", "opcnamespace"),
    "indcollation": _qualified(
        "i.indcollation::pg_catalog.oid[]", "pg_collation", "collname", "collnamespace"
    ),
}
NORMALIZED: dict[str, frozenset[str]] = {
    "pg_class": frozenset(_CLASS_NORMALIZED),
    "pg_index": frozenset(_INDEX_NORMALIZED),
    "pg_attribute": frozenset(_ATTRIBUTE_NORMALIZED),
    "pg_type": frozenset(_TYPE_NORMALIZED),
}
"""Catalog columns compared through a normalization rather than as stored."""

_CLASS_WHOLE = _whole("c", MEMBER_EXCLUSIONS["pg_class"], _CLASS_NORMALIZED)
_INDEX_WHOLE = _whole("i", MEMBER_EXCLUSIONS["pg_index"], _INDEX_NORMALIZED)
_ATTRIBUTE_WHOLE = _whole("a", MEMBER_EXCLUSIONS["pg_attribute"], _ATTRIBUTE_NORMALIZED)
_TYPE_WHOLE = _whole("y", MEMBER_EXCLUSIONS["pg_type"], _TYPE_NORMALIZED)


_QUERIES = {
    "relations": _SCOPE + "SELECT t.name, c.relkind::pg_catalog.text, c.relpersistence::pg_catalog.text,"
    f" (SELECT am.amname::pg_catalog.text FROM pg_catalog.pg_am am WHERE am.oid {_EQ} c.relam),"
    " c.reloptions::pg_catalog.text[], c.reltablespace::pg_catalog.int8,"
    " c.reloftype::pg_catalog.int8, c.relispartition, c.relrowsecurity, c.relforcerowsecurity,"
    " c.relreplident::pg_catalog.text,"
    " (SELECT tc.reloptions::pg_catalog.text[] FROM pg_catalog.pg_class tc"
    f" WHERE tc.oid {_EQ} c.reltoastrelid),"
    " c.relisshared, c.relispopulated, c.relrewrite::pg_catalog.int8, c.relpartbound IS NULL,"
    f" {_relnatts('c')}, {_relchecks('c')}"
    f" FROM t JOIN pg_catalog.pg_class c ON c.oid {_EQ} t.oid",
    "inheritance": _SCOPE
    + "SELECT i.inhrelid::pg_catalog.text, i.inhparent::pg_catalog.text FROM pg_catalog.pg_inherits i"
    f" WHERE i.inhrelid {_IN_SCOPE} OR i.inhparent {_IN_SCOPE}",
    "policies": _SCOPE + "SELECT t.name, p.polname::pg_catalog.text FROM pg_catalog.pg_policy p"
    f" JOIN t ON t.oid {_EQ} p.polrelid",
    "rules": _SCOPE + "SELECT t.name, r.rulename::pg_catalog.text FROM pg_catalog.pg_rewrite r"
    f" JOIN t ON t.oid {_EQ} r.ev_class",
    "triggers": _SCOPE
    + "SELECT t.name, g.tgisinternal, g.tgenabled::pg_catalog.text, pn.nspname::pg_catalog.text,"
    " p.proname::pg_catalog.text, g.tgtype::pg_catalog.int4, con.contype::pg_catalog.text,"
    " cr.relname::pg_catalog.text, cf.relname::pg_catalog.text, tr.relname::pg_catalog.text,"
    " g.tgdeferrable, g.tginitdeferred, g.tgnargs::pg_catalog.int4,"
    " pg_catalog.cardinality(g.tgattr::pg_catalog.int2[]), g.tgqual IS NULL,"
    f" con.conrelid {_IN_SCOPE}"
    f" FROM pg_catalog.pg_trigger g JOIN t ON t.oid {_EQ} g.tgrelid"
    f" JOIN pg_catalog.pg_proc p ON p.oid {_EQ} g.tgfoid"
    f" JOIN pg_catalog.pg_namespace pn ON pn.oid {_EQ} p.pronamespace"
    f" LEFT JOIN pg_catalog.pg_constraint con ON con.oid {_EQ} g.tgconstraint"
    f" LEFT JOIN pg_catalog.pg_class cr ON cr.oid {_EQ} con.conrelid"
    f" LEFT JOIN pg_catalog.pg_class cf ON cf.oid {_EQ} con.confrelid"
    f" LEFT JOIN pg_catalog.pg_class tr ON tr.oid {_EQ} g.tgconstrrelid",
    "columns": _SCOPE + "SELECT t.name, a.attnum::pg_catalog.int4, a.attname::pg_catalog.text,"
    " tn.nspname::pg_catalog.text, ty.typname::pg_catalog.text, ty.typtype::pg_catalog.text,"
    " a.atttypmod, a.attnotnull, pg_catalog.concat(cn.nspname, '.', co.collname),"
    " a.attidentity::pg_catalog.text, a.attgenerated::pg_catalog.text,"
    " a.attstorage::pg_catalog.text, a.attcompression::pg_catalog.text, a.atthasdef,"
    " a.attndims::pg_catalog.int4, a.attislocal, a.attinhcount::pg_catalog.int4,"
    " a.atthasmissing, a.attmissingval::pg_catalog.text, a.attfdwoptions::pg_catalog.text[],"
    f" a.attcacheoff::pg_catalog.int4, a.attlen {_EQ} ty.typlen, a.attbyval {_EQ} ty.typbyval,"
    f" a.attalign {_EQ} ty.typalign"
    f" FROM t JOIN pg_catalog.pg_attribute a ON a.attrelid {_EQ} t.oid"
    f" JOIN pg_catalog.pg_type ty ON ty.oid {_EQ} a.atttypid"
    f" JOIN pg_catalog.pg_namespace tn ON tn.oid {_EQ} ty.typnamespace"
    f" LEFT JOIN pg_catalog.pg_collation co ON co.oid {_EQ} a.attcollation"
    f" LEFT JOIN pg_catalog.pg_namespace cn ON cn.oid {_EQ} co.collnamespace"
    " WHERE a.attnum OPERATOR(pg_catalog.>) 0 AND NOT a.attisdropped",
    "defaults": _SCOPE
    + "SELECT t.name, a.attname::pg_catalog.text, d.adbin::pg_catalog.text FROM pg_catalog.pg_attrdef d"
    f" JOIN t ON t.oid {_EQ} d.adrelid JOIN pg_catalog.pg_attribute a ON a.attrelid {_EQ} d.adrelid"
    f" AND a.attnum {_EQ} d.adnum",
    "constraints": _SCOPE
    + "SELECT cr.relname::pg_catalog.text, con.conrelid {in}, con.contype::pg_catalog.text,"
    " con.condeferrable, con.condeferred, con.convalidated, con.connoinherit, con.conislocal,"
    " con.coninhcount::pg_catalog.int4, {key}, cf.relname::pg_catalog.text, con.confrelid {in},"
    " {fkey}, con.confupdtype::pg_catalog.text, con.confdeltype::pg_catalog.text,"
    " con.confmatchtype::pg_catalog.text, con.conbin::pg_catalog.text"
    " FROM pg_catalog.pg_constraint con"
    f" LEFT JOIN pg_catalog.pg_class cr ON cr.oid {_EQ} con.conrelid"
    f" LEFT JOIN pg_catalog.pg_class cf ON cf.oid {_EQ} con.confrelid"
    " WHERE con.conrelid {in} OR con.confrelid {in}",
    "indexes": _SCOPE + "SELECT t.name, i.indisunique, i.indisprimary, i.indisexclusion, i.indimmediate,"
    " i.indisclustered, i.indisvalid, i.indisready, i.indislive, i.indisreplident,"
    " i.indnatts::pg_catalog.int4, i.indnkeyatts::pg_catalog.int4, {ikey}, {opclass}, {collation},"
    " i.indoption::pg_catalog.int2[], i.indexprs IS NULL, i.indpred IS NULL,"
    " am.amname::pg_catalog.text, ic.reloptions::pg_catalog.text[], ic.reltablespace::pg_catalog.int8,"
    " ARRAY(SELECT con.contype::pg_catalog.text FROM pg_catalog.pg_constraint con"
    f" WHERE con.conindid {_EQ} i.indexrelid AND con.conrelid {_EQ} i.indrelid),"
    " ic.relkind::pg_catalog.text, ic.relpersistence::pg_catalog.text, ic.reloftype::pg_catalog.int8,"
    " ic.relisshared, ic.relrowsecurity, ic.relforcerowsecurity, ic.relispopulated,"
    " ic.relreplident::pg_catalog.text, ic.relispartition, ic.relrewrite::pg_catalog.int8,"
    " ic.relpartbound IS NULL,"
    f" {_relnatts('ic')}, {_relchecks('ic')}"
    f" FROM t JOIN pg_catalog.pg_index i ON i.indrelid {_EQ} t.oid"
    f" JOIN pg_catalog.pg_class ic ON ic.oid {_EQ} i.indexrelid"
    f" JOIN pg_catalog.pg_am am ON am.oid {_EQ} ic.relam",
    # One row per attribute of each index, keyed by its table, backing constraints and key columns.
    "index_columns": _SCOPE + "SELECT t.name, ARRAY(SELECT con.contype::pg_catalog.text"
    f" FROM pg_catalog.pg_constraint con WHERE con.conindid {_EQ} i.indexrelid"
    f" AND con.conrelid {_EQ} i.indrelid), {{ikey}}, a.attnum::pg_catalog.int4, ATTRIBUTE::pg_catalog.text"
    f" FROM t JOIN pg_catalog.pg_index i ON i.indrelid {_EQ} t.oid"
    f" JOIN pg_catalog.pg_attribute a ON a.attrelid {_EQ} i.indexrelid",
}
_QUERIES["constraints"] = _QUERIES["constraints"].format(
    **{
        "in": _IN_SCOPE,
        "key": _names("con.conkey", "con.conrelid"),
        "fkey": _names("con.confkey", "con.confrelid"),
    }
)
_QUERIES["indexes"] = _QUERIES["indexes"].format(
    ikey=_names("i.indkey::pg_catalog.int2[]", "i.indrelid"),
    opclass=_qualified("i.indclass::pg_catalog.oid[]", "pg_opclass", "opcname", "opcnamespace"),
    collation=_qualified("i.indcollation::pg_catalog.oid[]", "pg_collation", "collname", "collnamespace"),
)
_QUERIES["index_columns"] = (
    _QUERIES["index_columns"]
    .format(ikey=_names("i.indkey::pg_catalog.int2[]", "i.indrelid"))
    .replace("ATTRIBUTE", _ATTRIBUTE_WHOLE)
)
_JSON = "::pg_catalog.text"
_QUERIES["members"] = (
    _SCOPE + _ISLAND + f"SELECT tx.name, 'toast', {_CLASS_WHOLE}{_JSON} FROM tx"
    f" JOIN pg_catalog.pg_class c ON c.oid {_EQ} tx.oid"
    f" UNION ALL SELECT tx.name, 'toast column', {_ATTRIBUTE_WHOLE}{_JSON} FROM tx"
    f" JOIN pg_catalog.pg_attribute a ON a.attrelid {_EQ} tx.oid WHERE a.attnum OPERATOR(pg_catalog.>) 0"
    f" UNION ALL SELECT txi.name, 'toast index',"
    f" ({_CLASS_WHOLE} OPERATOR(pg_catalog.||) {_INDEX_WHOLE}){_JSON}"
    f" FROM txi JOIN pg_catalog.pg_class c ON c.oid {_EQ} txi.oid"
    f" JOIN pg_catalog.pg_index i ON i.indexrelid {_EQ} txi.oid"
    f" UNION ALL SELECT txi.name, 'toast index column', {_ATTRIBUTE_WHOLE}{_JSON} FROM txi"
    f" JOIN pg_catalog.pg_attribute a ON a.attrelid {_EQ} txi.oid WHERE a.attnum OPERATOR(pg_catalog.>) 0"
    f" UNION ALL SELECT ty.name, ty.kind, {_TYPE_WHOLE}{_JSON} FROM ty"
    f" JOIN pg_catalog.pg_type y ON y.oid {_EQ} ty.oid"
)
_QUERIES["boundary"] = (
    _SCOPE
    + _ISLAND
    # An outside object recording a dependency on the island (planner statistics exempt, G-094) ...
    + "SELECT 'inbound', dc.relname::pg_catalog.text, d.deptype::pg_catalog.text, rc.relname::pg_catalog.text"
    " FROM pg_catalog.pg_depend d"
    f" JOIN island r ON r.classid {_EQ} d.refclassid AND r.objid {_EQ} d.refobjid"
    f" JOIN pg_catalog.pg_class dc ON dc.oid {_EQ} d.classid"
    f" JOIN pg_catalog.pg_class rc ON rc.oid {_EQ} d.refclassid"
    f" WHERE NOT {_member('d.classid', 'd.objid')} AND d.classid {_NE} {_catalog('pg_statistic_ext')}"
    # ... an island object depending on anything but the island, the namespace and built-ins ...
    " UNION ALL SELECT 'outbound', dc.relname::pg_catalog.text, d.deptype::pg_catalog.text,"
    " rc.relname::pg_catalog.text FROM pg_catalog.pg_depend d"
    f" JOIN island o ON o.classid {_EQ} d.classid AND o.objid {_EQ} d.objid"
    f" JOIN pg_catalog.pg_class dc ON dc.oid {_EQ} d.classid"
    f" JOIN pg_catalog.pg_class rc ON rc.oid {_EQ} d.refclassid"
    f" WHERE NOT {_member('d.refclassid', 'd.refobjid')}"
    f" AND d.refobjid OPERATOR(pg_catalog.>=) {FIRST_NORMAL_OBJECT_ID}::pg_catalog.oid"
    f" AND NOT (d.refclassid {_EQ} {_catalog('pg_namespace')} AND d.refobjid {_EQ} %(ns)s::pg_catalog.oid)"
    # ... and a security label on the island (G-097).
    " UNION ALL SELECT 'label', l.provider::pg_catalog.text, '', lc.relname::pg_catalog.text"
    " FROM pg_catalog.pg_seclabel l"
    f" JOIN island o ON o.classid {_EQ} l.classoid AND o.objid {_EQ} l.objoid"
    f" JOIN pg_catalog.pg_class lc ON lc.oid {_EQ} l.classoid"
)
QUERIES: dict[str, str] = dict(_QUERIES)
"""Every catalog query open issues, by part (exposed for the CH12 poisoned-session control)."""

_LOCATION = re.compile(r" :location -?\d+")
_VARATTNO = re.compile(r":(varattno|varattnosyn) (-?\d+)")


def _tree(tree: str | None, names: dict[int, str]) -> str | None:
    """A stored expression tree with source locations removed and column numbers named."""
    if tree is None:
        return None
    tree = _LOCATION.sub("", tree)
    return _VARATTNO.sub(lambda m: f":{m.group(1)} {names.get(int(m.group(2)), m.group(2))}", tree)


def _rows(rows: list[tuple[Any, ...]]) -> Rows:
    def plain(value: Any) -> Any:
        return tuple(plain(v) for v in value) if isinstance(value, list) else value

    return tuple(sorted((tuple(plain(v) for v in row) for row in rows), key=repr))


def fetch(connection: psycopg.Connection[Any], namespace_oid: str) -> Catalog:
    """Read the catalog state of the namespace's `mneme_` relations."""
    raw = {part: connection.execute(sql, {"ns": namespace_oid}).fetchall() for part, sql in QUERIES.items()}
    attnames: dict[str, dict[int, str]] = {}
    for row in raw["columns"]:
        attnames.setdefault(row[0], {})[row[1]] = row[2]
    columns = [row[:1] + row[2:] for row in raw["columns"]]  # column order is non-semantic (G-089)
    defaults = [(t, c, _tree(tree, attnames.get(t, {}))) for t, c, tree in raw["defaults"]]
    constraints = [
        (*row[:-1], _tree(row[-1], attnames.get(row[0], {}) if row[1] else {})) for row in raw["constraints"]
    ]
    # Inheritance and policies only need to be absent; their rows are kept OID-free.
    return Catalog(
        relations=_rows(raw["relations"]),
        inheritance=_rows([("inherits",) for _ in raw["inheritance"]]),
        policies=_rows(raw["policies"]),
        rules=_rows(raw["rules"]),
        triggers=_rows(raw["triggers"]),
        columns=_rows(columns),
        defaults=_rows(defaults),
        constraints=_rows(constraints),
        indexes=_rows(raw["indexes"]),
        index_columns=_rows(raw["index_columns"]),
        members=_rows(raw["members"]),
        boundary=_rows(raw["boundary"]),
    )
