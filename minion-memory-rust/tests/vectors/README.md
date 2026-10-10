# Independent binary64 controls

`binary64.json` was generated with Node **22.15.1**, using ECMAScript `JSON.stringify` and the
SER-008 finite/integral admission rule, without importing either Mneme implementation.
It covers 512 fixed/seeded binary64 bit patterns (seed `71eafb934f281ba5`) and 24-significant-digit
decimal spellings for finite patterns. The Rust control verifies both native numbers and text.

Reproduce from the Rust crate directory:

```text
node tests/vectors/generate.cjs tests/vectors/binary64.json
```

SHA-256 of the committed fixture:
`4cda345fb58d7b9fe5a4686623571d4129869697bf15ea91651eff253ff98dda`.
