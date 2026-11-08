# Signing fixtures

## `signer.pfx` — a throwaway test identity, not a secret

A single self-signed RSA-2048 certificate with its private key, sealed in a PKCS#12 archive.

| | |
|---|---|
| Subject | `CN=DocReader Test Signer, O=DocReader Test, C=GB` |
| Password | `correct-horse` (published here on purpose) |
| Encryption | PBES2 / PBKDF2 / AES-256-CBC for both the certificate and the shrouded key bag, MAC `sha256`, iteration 2048 |
| SHA-256 | `120d0859e932fdf2dd1d7acaa749a86ab775b0ff5cde03b35d669a934c10ecff` |

Regenerate with:

```bash
openssl req -x509 -newkey rsa:2048 -keyout /tmp/si.key -out /tmp/si.crt \
  -days 3650 -nodes -subj "/CN=DocReader Test Signer/O=DocReader Test/C=GB"
openssl pkcs12 -export -out signer.pfx -inkey /tmp/si.key -in /tmp/si.crt \
  -passout pass:correct-horse \
  -certpbe AES-256-CBC -keypbe AES-256-CBC -macalg sha256
```

## Why a private key is in the repository

This is the first key-bearing file committed to this tree, so the reasoning is recorded rather
than left to a later reader to infer.

The PFX reader has to be tested against the real shape, not a hand-built approximation: the whole
defect it replaced (`pkcs12` cannot decrypt at all, design K22) would have been invisible to a
test that only exercised the ASN.1 types. A committed fixture keeps that test hermetic — no
`openssl` on the CI runner, no generation step that could drift from the committed bytes, and
therefore no way for the test and the fixture to disagree.

The cost is accepted deliberately and bounded:

* the key is **self-signed and chains to no trust anchor**; it is not, and cannot become, a
  credential for anything. No CA vouches for it and nothing it signs can authenticate to a third
  party;
* the password is published in this file. A password that is in the repository protects nothing,
  and pretending otherwise — a "secret" in an environment variable on a machine with no secret
  store — would be worse than publishing it;
* it must never be used to sign anything a person relies on. Nothing in the product loads a
  fixture; only tests do, from this directory.

A test that signs with it proves the *mechanism*. It never claims the resulting signature carries
any trust — that is D-SG4's separate question, and the answer for this key is `unknown`.

## `keyonly.pfx` — a key with no certificate

Exported with `openssl pkcs12 -export -nocerts`, same password and encryption as `signer.pfx`.

It exists to pin a measured behaviour rather than a guessed one: under the reader's `Strict` policy
the key links to no certificate, is dropped, and the import **succeeds with zero entries**. Without
a fixture for it, "the keystore opened and holds nothing" would be a silent no-op — the importer
would return an empty identity list and no reason. The test asserts the refusal and its wording, so
the empty result cannot come back as a success.
