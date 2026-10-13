//! Structured PostgreSQL 14 v1 catalog observations and shared pins (G-089..G-098).
//! Query results are OID-free row arrays; member documents retain PostgreSQL jsonb text.
//! This module's connection seam supports bounded fetch-binding controls, not repository writes.

use super::StoreError;
use crate::values::Node;
use crate::{Error, Value, canonical_bytes, decode_json};
use postgres::{Client, Transaction};
use regex::Regex;
use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;

/// Accepted cross-language reference data, embedded without a language-specific copy.
pub const SHARED_PINS: &str = include_str!("../../../reference-backend/postgres14-v1-catalog.json");
/// Fixed diagnostic comparison order from the agreed checkpoint.
pub const PARTS: [&str; 12] = [
    "relations",
    "inheritance",
    "policies",
    "rules",
    "triggers",
    "columns",
    "defaults",
    "constraints",
    "indexes",
    "index_columns",
    "members",
    "boundary",
];

/// A connection returning the JSON row texts emitted by actual catalog SQL.
/// A test proxy may project a catalog field; it must not replace the fetch algorithm.
pub trait CatalogConnection {
    /// Execute a real catalog statement in the supplied resolved namespace.
    fn catalog_rows(&mut self, query: &str, namespace: u32)
    -> Result<Vec<String>, postgres::Error>;
}
impl CatalogConnection for Client {
    fn catalog_rows(
        &mut self,
        query: &str,
        namespace: u32,
    ) -> Result<Vec<String>, postgres::Error> {
        Ok(self
            .query(query, &[&namespace])?
            .iter()
            .map(|r| r.get(0))
            .collect())
    }
}
impl CatalogConnection for Transaction<'_> {
    fn catalog_rows(
        &mut self,
        query: &str,
        namespace: u32,
    ) -> Result<Vec<String>, postgres::Error> {
        Ok(self
            .query(query, &[&namespace])?
            .iter()
            .map(|r| r.get(0))
            .collect())
    }
}

/// Rows grouped by the checkpoint's parts. Order is non-semantic, multiplicity is preserved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalog {
    rows: BTreeMap<String, Vec<Vec<u8>>>,
}
impl Catalog {
    /// Construct a comparison input from structured JSON row arrays (predicate seam).
    pub fn from_rows(rows: BTreeMap<String, Vec<String>>) -> Result<Self, Error> {
        let mut result = BTreeMap::new();
        for (part, rows) in rows {
            let mut normalized = rows
                .into_iter()
                .map(|r| canonical_bytes(&decode_json(r.as_bytes())?))
                .collect::<Result<Vec<_>, _>>()?;
            normalized.sort();
            result.insert(part, normalized);
        }
        Ok(Self { rows: result })
    }
    /// Structured row texts, for metadata inventory and pure predicate witnesses.
    pub fn rows(&self) -> BTreeMap<String, Vec<String>> {
        self.rows
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    v.iter()
                        .map(|r| String::from_utf8(r.clone()).expect("canonical UTF8"))
                        .collect(),
                )
            })
            .collect()
    }
    /// First part differing from the accepted pins, including unexpected/missing parts.
    pub fn mismatch(&self) -> Result<Option<String>, Error> {
        Ok(self.mismatch_with(pins()))
    }
    /// Pure comparison against a second complete catalog, independent of fetch.
    pub fn mismatch_with(&self, expected: &Self) -> Option<String> {
        for part in PARTS {
            if self.rows.get(part) != expected.rows.get(part) {
                return Some(part.to_owned());
            }
        }
        if self.rows.len() != expected.rows.len() {
            return Some("inventory".to_owned());
        }
        None
    }
    pub(crate) fn empty_namespace(&self) -> bool {
        self.rows.get("relations").is_some_and(Vec::is_empty)
    }
}

static PINS: LazyLock<Catalog> = LazyLock::new(|| {
    let value = decode_json(SHARED_PINS.as_bytes()).expect("accepted shared pins");
    let Node::Object(root) = value.node() else {
        panic!("shared pins object")
    };
    let (_, catalog) = root
        .iter()
        .find(|(k, _)| k == "catalog")
        .expect("catalog key");
    let Node::Object(parts) = &value.nodes[*catalog] else {
        panic!("catalog object")
    };
    let mut rows = BTreeMap::new();
    for (part, index) in parts {
        let Node::Array(items) = &value.nodes[*index] else {
            panic!("part rows")
        };
        rows.insert(
            part.clone(),
            items
                .iter()
                .map(|i| {
                    String::from_utf8(canonical_bytes(&value.detached(*i)).expect("pin row"))
                        .expect("UTF8")
                })
                .collect(),
        );
    }
    Catalog::from_rows(rows).expect("accepted row domain")
});
/// The common v1 initializer state, never inferred from a language's implementation.
pub fn pins() -> &'static Catalog {
    &PINS
}
/// G-090/G-091 environmental predicate, kept separate from catalog fetching.
pub fn environment_matches(version: i32, encoding: &str, collate: &str, ctype: &str) -> bool {
    version / 10000 == 14
        && encoding == "UTF8"
        && matches!(collate, "C" | "POSIX")
        && matches!(ctype, "C" | "POSIX")
}

const EQ: &str = "OPERATOR(pg_catalog.=)";
const NE: &str = "OPERATOR(pg_catalog.<>)";
const SCOPE: &str = "WITH t AS (SELECT c.oid,c.relname::pg_catalog.text AS name FROM pg_catalog.pg_class c WHERE c.relnamespace OPERATOR(pg_catalog.=) $1::pg_catalog.oid AND c.relname OPERATOR(pg_catalog.~) '^mneme_' AND c.relkind OPERATOR(pg_catalog.<>) 'i' AND c.relkind OPERATOR(pg_catalog.<>) 'I') ";
const INSIDE: &str = "OPERATOR(pg_catalog.=) ANY(SELECT oid FROM t)";
const ISLAND: &str = r#",
tx AS (SELECT t.name,c.reltoastrelid AS oid FROM t JOIN pg_catalog.pg_class c ON c.oid OPERATOR(pg_catalog.=) t.oid WHERE c.reltoastrelid OPERATOR(pg_catalog.<>) 0),
txi AS (SELECT tx.name,i.indexrelid AS oid FROM tx JOIN pg_catalog.pg_index i ON i.indrelid OPERATOR(pg_catalog.=) tx.oid),
ty AS (SELECT t.name,'row' AS kind,c.reltype AS oid,t.oid AS relid,0::pg_catalog.oid AS elem,y.typarray AS arr FROM t JOIN pg_catalog.pg_class c ON c.oid OPERATOR(pg_catalog.=) t.oid JOIN pg_catalog.pg_type y ON y.oid OPERATOR(pg_catalog.=) c.reltype
 UNION ALL SELECT t.name,'array',y.typarray,0::pg_catalog.oid,c.reltype,0::pg_catalog.oid FROM t JOIN pg_catalog.pg_class c ON c.oid OPERATOR(pg_catalog.=) t.oid JOIN pg_catalog.pg_type y ON y.oid OPERATOR(pg_catalog.=) c.reltype WHERE y.typarray OPERATOR(pg_catalog.<>) 0),
island(classid,objid) AS (
 SELECT 'pg_catalog.pg_class'::pg_catalog.regclass::pg_catalog.oid,t.oid FROM t
 UNION ALL SELECT 'pg_catalog.pg_class'::pg_catalog.regclass::pg_catalog.oid,i.indexrelid FROM pg_catalog.pg_index i WHERE i.indrelid OPERATOR(pg_catalog.=) ANY(SELECT oid FROM t)
 UNION ALL SELECT 'pg_catalog.pg_class'::pg_catalog.regclass::pg_catalog.oid,tx.oid FROM tx
 UNION ALL SELECT 'pg_catalog.pg_class'::pg_catalog.regclass::pg_catalog.oid,txi.oid FROM txi
 UNION ALL SELECT 'pg_catalog.pg_type'::pg_catalog.regclass::pg_catalog.oid,ty.oid FROM ty
 UNION ALL SELECT 'pg_catalog.pg_constraint'::pg_catalog.regclass::pg_catalog.oid,k.oid FROM pg_catalog.pg_constraint k WHERE k.conrelid OPERATOR(pg_catalog.=) ANY(SELECT oid FROM t)
 UNION ALL SELECT 'pg_catalog.pg_attrdef'::pg_catalog.regclass::pg_catalog.oid,d.oid FROM pg_catalog.pg_attrdef d WHERE d.adrelid OPERATOR(pg_catalog.=) ANY(SELECT oid FROM t)
 UNION ALL SELECT 'pg_catalog.pg_trigger'::pg_catalog.regclass::pg_catalog.oid,g.oid FROM pg_catalog.pg_trigger g WHERE g.tgrelid OPERATOR(pg_catalog.=) ANY(SELECT oid FROM t)) "#;

/// Closed per-catalog whole-row exclusion list, checkpoint §10.1.
pub fn exclusions(catalog: &str) -> &'static [&'static str] {
    match catalog {
        "pg_class" => &[
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
        ],
        "pg_index" => &["indexrelid", "indrelid", "indcheckxmin"],
        "pg_attribute" => &[
            "attrelid",
            "attname",
            "attacl",
            "attstattarget",
            "attoptions",
        ],
        "pg_type" => &["oid", "typname", "typowner", "typacl"],
        _ => &[],
    }
}
fn array(fields: &[String]) -> String {
    format!(
        "pg_catalog.jsonb_build_array({})::pg_catalog.text",
        fields.join(",")
    )
}
fn strings(fields: &str) -> Vec<String> {
    fields.split('|').map(str::to_owned).collect()
}
fn whole(alias: &str, excluded: &[&str], normal: Vec<(&str, String)>) -> String {
    let keys = excluded
        .iter()
        .copied()
        .chain(normal.iter().map(|(k, _)| *k))
        .map(|k| format!("'{k}'"))
        .collect::<Vec<_>>()
        .join(",");
    let pairs = normal
        .into_iter()
        .map(|(k, v)| format!("'{k}',{v}"))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "(pg_catalog.to_jsonb({alias}) OPERATOR(pg_catalog.-) ARRAY[{keys}]::pg_catalog.text[] OPERATOR(pg_catalog.||) pg_catalog.jsonb_build_object({pairs}))"
    )
}
fn name_of(oid: &str, table: &str, name: &str, namespace: &str) -> String {
    format!(
        "(SELECT pg_catalog.concat(n.nspname,'.',o.{name}) FROM pg_catalog.{table} o JOIN pg_catalog.pg_namespace n ON n.oid {EQ} o.{namespace} WHERE o.oid {EQ} {oid})"
    )
}
fn qualified_vector(vector: &str, table: &str, name: &str, namespace: &str) -> String {
    format!(
        "ARRAY(SELECT pg_catalog.concat(n.nspname,'.',o.{name}) FROM pg_catalog.unnest({vector}::pg_catalog.oid[]) WITH ORDINALITY AS u(oid,ordinal) LEFT JOIN pg_catalog.{table} o ON o.oid {EQ} u.oid LEFT JOIN pg_catalog.pg_namespace n ON n.oid {EQ} o.{namespace} ORDER BY u.ordinal)"
    )
}
fn column_names(vector: &str, relation: &str) -> String {
    format!(
        "ARRAY(SELECT a.attname::pg_catalog.text FROM pg_catalog.unnest({vector}::pg_catalog.int2[]) WITH ORDINALITY AS u(num,ordinal) LEFT JOIN pg_catalog.pg_attribute a ON a.attrelid {EQ} {relation} AND a.attnum {EQ} u.num ORDER BY u.ordinal)"
    )
}
fn counts(alias: &str) -> [String; 2] {
    [
        format!(
            "{alias}.relnatts {EQ} (SELECT COALESCE(pg_catalog.max(a.attnum),0) FROM pg_catalog.pg_attribute a WHERE a.attrelid {EQ} {alias}.oid AND a.attnum OPERATOR(pg_catalog.>) 0)"
        ),
        format!(
            "{alias}.relchecks::pg_catalog.int8 {EQ} (SELECT pg_catalog.count(*) FROM pg_catalog.pg_constraint k WHERE k.conrelid {EQ} {alias}.oid AND k.contype {EQ} 'c')"
        ),
    ]
}
fn class_document() -> String {
    let [natts, checks] = counts("c");
    whole("c",exclusions("pg_class"),vec![
        ("relnamespace","(SELECT n.nspname::pg_catalog.text FROM pg_catalog.pg_namespace n WHERE n.oid OPERATOR(pg_catalog.=) c.relnamespace)".into()),
        ("relam","(SELECT am.amname::pg_catalog.text FROM pg_catalog.pg_am am WHERE am.oid OPERATOR(pg_catalog.=) c.relam)".into()),
        ("reltype",format!("c.reltype {NE} 0")),("relnatts",natts),("relchecks",checks),
    ])
}
fn attribute_document() -> String {
    whole(
        "a",
        exclusions("pg_attribute"),
        vec![
            (
                "atttypid",
                name_of("a.atttypid", "pg_type", "typname", "typnamespace"),
            ),
            (
                "attcollation",
                name_of(
                    "a.attcollation",
                    "pg_collation",
                    "collname",
                    "collnamespace",
                ),
            ),
            (
                "attlen",
                format!(
                    "(SELECT a.attlen {EQ} y.typlen FROM pg_catalog.pg_type y WHERE y.oid {EQ} a.atttypid)"
                ),
            ),
            (
                "attbyval",
                format!(
                    "(SELECT a.attbyval {EQ} y.typbyval FROM pg_catalog.pg_type y WHERE y.oid {EQ} a.atttypid)"
                ),
            ),
            (
                "attalign",
                format!(
                    "(SELECT a.attalign {EQ} y.typalign FROM pg_catalog.pg_type y WHERE y.oid {EQ} a.atttypid)"
                ),
            ),
        ],
    )
}
fn index_document() -> String {
    whole(
        "i",
        exclusions("pg_index"),
        vec![
            (
                "indclass",
                qualified_vector("i.indclass", "pg_opclass", "opcname", "opcnamespace"),
            ),
            (
                "indcollation",
                qualified_vector(
                    "i.indcollation",
                    "pg_collation",
                    "collname",
                    "collnamespace",
                ),
            ),
        ],
    )
}
fn type_document() -> String {
    let mut fields = vec![
        (
            "typnamespace",
            format!("y.typnamespace {EQ} $1::pg_catalog.oid"),
        ),
        ("typrelid", format!("y.typrelid {EQ} ty.relid")),
        ("typelem", format!("y.typelem {EQ} ty.elem")),
        ("typarray", format!("y.typarray {EQ} ty.arr")),
    ];
    for f in [
        "typinput",
        "typoutput",
        "typreceive",
        "typsend",
        "typmodin",
        "typmodout",
        "typanalyze",
        "typsubscript",
    ] {
        fields.push((
            f,
            name_of(
                &format!("y.{f}::pg_catalog.oid"),
                "pg_proc",
                "proname",
                "pronamespace",
            ),
        ));
    }
    whole("y", exclusions("pg_type"), fields)
}
fn backing() -> String {
    format!(
        "ARRAY(SELECT k.contype::pg_catalog.text FROM pg_catalog.pg_constraint k WHERE k.conindid {EQ} i.indexrelid AND k.conrelid {EQ} i.indrelid ORDER BY k.contype)"
    )
}
fn in_island(classid: &str, objid: &str) -> String {
    format!(
        "EXISTS(SELECT 1 FROM island s WHERE s.classid {EQ} {classid} AND s.objid {EQ} {objid})"
    )
}

/// Actual SQL statements. Every statement returns one JSON-array text per row.
/// Exposed so engine/fetch controls can act on Rust's SQL, not a second algorithm.
pub fn queries() -> BTreeMap<String, String> {
    let mut q = BTreeMap::new();
    let [natts, checks] = counts("c");
    let mut f = strings(
        "t.name|c.relkind::pg_catalog.text|c.relpersistence::pg_catalog.text|am.amname::pg_catalog.text|c.reloptions|c.reltablespace::pg_catalog.int8|c.reloftype::pg_catalog.int8|c.relispartition|c.relrowsecurity|c.relforcerowsecurity|c.relreplident::pg_catalog.text|toast.reloptions|c.relisshared|c.relispopulated|c.relrewrite::pg_catalog.int8|c.relpartbound IS NULL",
    );
    f.extend([natts, checks]);
    q.insert("relations".into(),format!("{SCOPE}SELECT {} FROM t JOIN pg_catalog.pg_class c ON c.oid {EQ} t.oid LEFT JOIN pg_catalog.pg_am am ON am.oid {EQ} c.relam LEFT JOIN pg_catalog.pg_class toast ON toast.oid {EQ} c.reltoastrelid",array(&f)));
    q.insert("inheritance".into(),format!("{SCOPE}SELECT {} FROM pg_catalog.pg_inherits h WHERE h.inhrelid {INSIDE} OR h.inhparent {INSIDE}",array(&["'inherits'".into()])));
    for (part, table, rel, name) in [
        ("policies", "pg_policy", "polrelid", "polname"),
        ("rules", "pg_rewrite", "ev_class", "rulename"),
    ] {
        q.insert(
            part.into(),
            format!(
                "{SCOPE}SELECT {} FROM t JOIN pg_catalog.{table} x ON x.{rel} {EQ} t.oid",
                array(&strings(&format!("t.name|x.{name}::pg_catalog.text")))
            ),
        );
    }
    let mut f = strings(
        "t.name|g.tgisinternal|g.tgenabled::pg_catalog.text|pn.nspname::pg_catalog.text|p.proname::pg_catalog.text|g.tgtype::pg_catalog.int4|k.contype::pg_catalog.text|cr.relname::pg_catalog.text|cf.relname::pg_catalog.text|cg.relname::pg_catalog.text|g.tgdeferrable|g.tginitdeferred|g.tgnargs::pg_catalog.int4|pg_catalog.cardinality(g.tgattr::pg_catalog.int2[])|g.tgqual IS NULL",
    );
    f.push(format!("k.conrelid {INSIDE}"));
    q.insert("triggers".into(),format!("{SCOPE}SELECT {} FROM t JOIN pg_catalog.pg_trigger g ON g.tgrelid {EQ} t.oid JOIN pg_catalog.pg_proc p ON p.oid {EQ} g.tgfoid JOIN pg_catalog.pg_namespace pn ON pn.oid {EQ} p.pronamespace LEFT JOIN pg_catalog.pg_constraint k ON k.oid {EQ} g.tgconstraint LEFT JOIN pg_catalog.pg_class cr ON cr.oid {EQ} k.conrelid LEFT JOIN pg_catalog.pg_class cf ON cf.oid {EQ} k.confrelid LEFT JOIN pg_catalog.pg_class cg ON cg.oid {EQ} g.tgconstrrelid",array(&f)));
    let f = strings(
        "t.name|a.attname::pg_catalog.text|yn.nspname::pg_catalog.text|y.typname::pg_catalog.text|y.typtype::pg_catalog.text|a.atttypmod::pg_catalog.int4|a.attnotnull|pg_catalog.concat(cn.nspname,'.',co.collname)|a.attidentity::pg_catalog.text|a.attgenerated::pg_catalog.text|a.attstorage::pg_catalog.text|a.attcompression::pg_catalog.text|a.atthasdef|a.attndims::pg_catalog.int4|a.attislocal|a.attinhcount::pg_catalog.int4|a.atthasmissing|a.attmissingval::pg_catalog.text|a.attfdwoptions|a.attcacheoff|a.attlen OPERATOR(pg_catalog.=) y.typlen|a.attbyval OPERATOR(pg_catalog.=) y.typbyval|a.attalign OPERATOR(pg_catalog.=) y.typalign|a.attnum::pg_catalog.int4",
    );
    q.insert("columns".into(),format!("{SCOPE}SELECT {} FROM t JOIN pg_catalog.pg_attribute a ON a.attrelid {EQ} t.oid JOIN pg_catalog.pg_type y ON y.oid {EQ} a.atttypid JOIN pg_catalog.pg_namespace yn ON yn.oid {EQ} y.typnamespace LEFT JOIN pg_catalog.pg_collation co ON co.oid {EQ} a.attcollation LEFT JOIN pg_catalog.pg_namespace cn ON cn.oid {EQ} co.collnamespace WHERE a.attnum OPERATOR(pg_catalog.>) 0 AND NOT a.attisdropped",array(&f)));
    q.insert("defaults".into(),format!("{SCOPE}SELECT {} FROM t JOIN pg_catalog.pg_attrdef d ON d.adrelid {EQ} t.oid JOIN pg_catalog.pg_attribute a ON a.attrelid {EQ} d.adrelid AND a.attnum {EQ} d.adnum",array(&strings("t.name|a.attname::pg_catalog.text|d.adbin::pg_catalog.text"))));
    let mut f = vec![
        "cr.relname::pg_catalog.text".into(),
        format!("k.conrelid {INSIDE}"),
    ];
    f.extend(strings("k.contype::pg_catalog.text|k.condeferrable|k.condeferred|k.convalidated|k.connoinherit|k.conislocal|k.coninhcount::pg_catalog.int4"));
    f.extend([
        column_names("k.conkey", "k.conrelid"),
        "cf.relname::pg_catalog.text".into(),
        format!("k.confrelid {INSIDE}"),
        column_names("k.confkey", "k.confrelid"),
    ]);
    f.extend(strings("k.confupdtype::pg_catalog.text|k.confdeltype::pg_catalog.text|k.confmatchtype::pg_catalog.text|k.conbin::pg_catalog.text"));
    q.insert("constraints".into(),format!("{SCOPE}SELECT {} FROM pg_catalog.pg_constraint k LEFT JOIN pg_catalog.pg_class cr ON cr.oid {EQ} k.conrelid LEFT JOIN pg_catalog.pg_class cf ON cf.oid {EQ} k.confrelid WHERE k.conrelid {INSIDE} OR k.confrelid {INSIDE}",array(&f)));
    let mut f = strings(
        "t.name|i.indisunique|i.indisprimary|i.indisexclusion|i.indimmediate|i.indisclustered|i.indisvalid|i.indisready|i.indislive|i.indisreplident|i.indnatts::pg_catalog.int4|i.indnkeyatts::pg_catalog.int4",
    );
    f.extend([
        column_names("i.indkey", "i.indrelid"),
        qualified_vector("i.indclass", "pg_opclass", "opcname", "opcnamespace"),
        qualified_vector(
            "i.indcollation",
            "pg_collation",
            "collname",
            "collnamespace",
        ),
    ]);
    f.extend(strings("i.indoption::pg_catalog.int2[]|i.indexprs IS NULL|i.indpred IS NULL|am.amname::pg_catalog.text|c.reloptions|c.reltablespace::pg_catalog.int8"));
    f.push(backing());
    f.extend(strings("c.relkind::pg_catalog.text|c.relpersistence::pg_catalog.text|c.reloftype::pg_catalog.int8|c.relisshared|c.relrowsecurity|c.relforcerowsecurity|c.relispopulated|c.relreplident::pg_catalog.text|c.relispartition|c.relrewrite::pg_catalog.int8|c.relpartbound IS NULL"));
    f.extend(counts("c"));
    q.insert("indexes".into(),format!("{SCOPE}SELECT {} FROM t JOIN pg_catalog.pg_index i ON i.indrelid {EQ} t.oid JOIN pg_catalog.pg_class c ON c.oid {EQ} i.indexrelid LEFT JOIN pg_catalog.pg_am am ON am.oid {EQ} c.relam",array(&f)));
    q.insert("index_columns".into(),format!("{SCOPE}SELECT {} FROM t JOIN pg_catalog.pg_index i ON i.indrelid {EQ} t.oid JOIN pg_catalog.pg_attribute a ON a.attrelid {EQ} i.indexrelid WHERE a.attnum OPERATOR(pg_catalog.>) 0",array(&["t.name".into(),backing(),column_names("i.indkey","i.indrelid"),"a.attnum::pg_catalog.int4".into(),format!("{}::pg_catalog.text",attribute_document())])));
    let island = format!("{}{ISLAND}", SCOPE.trim_end());
    let cls = class_document();
    let attr = attribute_document();
    let idx = index_document();
    let ty = type_document();
    q.insert("members".into(),format!("{island}SELECT {} FROM tx JOIN pg_catalog.pg_class c ON c.oid {EQ} tx.oid UNION ALL SELECT {} FROM tx JOIN pg_catalog.pg_attribute a ON a.attrelid {EQ} tx.oid WHERE a.attnum OPERATOR(pg_catalog.>) 0 UNION ALL SELECT {} FROM txi JOIN pg_catalog.pg_class c ON c.oid {EQ} txi.oid JOIN pg_catalog.pg_index i ON i.indexrelid {EQ} txi.oid UNION ALL SELECT {} FROM txi JOIN pg_catalog.pg_attribute a ON a.attrelid {EQ} txi.oid WHERE a.attnum OPERATOR(pg_catalog.>) 0 UNION ALL SELECT {} FROM ty JOIN pg_catalog.pg_type y ON y.oid {EQ} ty.oid",
        array(&["tx.name".into(),"'toast'".into(),format!("{cls}::pg_catalog.text")]),
        array(&["tx.name".into(),"'toast column'".into(),format!("{attr}::pg_catalog.text")]),
        array(&["txi.name".into(),"'toast index'".into(),format!("({cls} OPERATOR(pg_catalog.||) {idx})::pg_catalog.text")]),
        array(&["txi.name".into(),"'toast index column'".into(),format!("{attr}::pg_catalog.text")]),
        array(&["ty.name".into(),"ty.kind".into(),format!("{ty}::pg_catalog.text")])));
    let inbound = in_island("d.classid", "d.objid");
    let outbound = in_island("d.refclassid", "d.refobjid");
    q.insert("boundary".into(),format!("{island}SELECT {} FROM pg_catalog.pg_depend d JOIN island r ON r.classid {EQ} d.refclassid AND r.objid {EQ} d.refobjid JOIN pg_catalog.pg_class dc ON dc.oid {EQ} d.classid JOIN pg_catalog.pg_class rc ON rc.oid {EQ} d.refclassid WHERE NOT {inbound} AND d.classid {NE} 'pg_catalog.pg_statistic_ext'::pg_catalog.regclass::pg_catalog.oid UNION ALL SELECT {} FROM pg_catalog.pg_depend d JOIN island o ON o.classid {EQ} d.classid AND o.objid {EQ} d.objid JOIN pg_catalog.pg_class dc ON dc.oid {EQ} d.classid JOIN pg_catalog.pg_class rc ON rc.oid {EQ} d.refclassid WHERE NOT {outbound} AND d.refobjid OPERATOR(pg_catalog.>=) 16384::pg_catalog.oid AND NOT(d.refclassid {EQ} 'pg_catalog.pg_namespace'::pg_catalog.regclass::pg_catalog.oid AND d.refobjid {EQ} $1::pg_catalog.oid) UNION ALL SELECT {} FROM pg_catalog.pg_seclabel l JOIN island o ON o.classid {EQ} l.classoid AND o.objid {EQ} l.objoid JOIN pg_catalog.pg_class lc ON lc.oid {EQ} l.classoid",
        array(&strings("'inbound'|dc.relname::pg_catalog.text|d.deptype::pg_catalog.text|rc.relname::pg_catalog.text")),
        array(&strings("'outbound'|dc.relname::pg_catalog.text|d.deptype::pg_catalog.text|rc.relname::pg_catalog.text")),
        array(&strings("'label'|l.provider::pg_catalog.text|''|lc.relname::pg_catalog.text"))));
    q
}

static LOCATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r" :location -?\d+").expect("constant regex"));
static ATTRIBUTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r":(varattno|varattnosyn) (-?\d+)").expect("constant regex"));
fn normalized_tree(tree: &str, names: Option<&HashMap<i128, String>>) -> String {
    let tree = LOCATION.replace_all(tree, "");
    ATTRIBUTE
        .replace_all(&tree, |capture: &regex::Captures<'_>| {
            let original = &capture[2];
            let name = original
                .parse()
                .ok()
                .and_then(|number| names.and_then(|names| names.get(&number)))
                .map_or(original, String::as_str);
            format!(":{} {name}", &capture[1])
        })
        .into_owned()
}
fn items(value: &Value) -> Vec<Value> {
    let Node::Array(children) = value.node() else {
        panic!("catalog SQL returns arrays")
    };
    children
        .iter()
        .map(|index| value.detached(*index))
        .collect()
}
/// Read all genuine structured parts, then normalize expression references and row order.
pub fn fetch(
    connection: &mut impl CatalogConnection,
    namespace: u32,
) -> Result<Catalog, StoreError> {
    let mut raw = BTreeMap::<String, Vec<Vec<Value>>>::new();
    let statements = queries();
    for part in PARTS {
        let rows = connection.catalog_rows(&statements[part], namespace)?;
        raw.insert(
            part.to_owned(),
            rows.iter()
                .map(|row| decode_json(row.as_bytes()).map(|value| items(&value)))
                .collect::<Result<_, _>>()?,
        );
    }
    let mut names = HashMap::<String, HashMap<i128, String>>::new();
    for row in raw.get_mut("columns").expect("part") {
        let ordinal = row
            .pop()
            .expect("fetch ordinal")
            .as_integer()
            .expect("attribute number");
        names
            .entry(row[0].as_str().expect("table").to_owned())
            .or_default()
            .insert(ordinal, row[1].as_str().expect("column").to_owned());
    }
    for part in ["defaults", "constraints"] {
        for row in raw.get_mut(part).expect("part") {
            let name = row[0].as_str().map(str::to_owned);
            let last = row.last_mut().expect("tree slot");
            if let Some(tree) = last.as_str() {
                *last = Value::string(&normalized_tree(
                    tree,
                    name.as_ref().and_then(|n| names.get(n)),
                ));
            }
        }
    }
    let mut rows = BTreeMap::new();
    for (part, values) in raw {
        rows.insert(
            part,
            values
                .into_iter()
                .map(|row| {
                    canonical_bytes(&Value::array(row))
                        .map(|bytes| String::from_utf8(bytes).expect("UTF8"))
                })
                .collect::<Result<_, _>>()?,
        );
    }
    Ok(Catalog::from_rows(rows)?)
}
