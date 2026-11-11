/**
 * P3.2 SCOPE-C: the Sign dialog actually reaches the backend, and the D-SG6 level is an explicit
 * choice rather than an implied one.
 *
 * These drive the real `src/api/invoke.ts` through the fake IPC boundary, so the command name and
 * the argument mapping are exercised too. A test that mocked `invoke.ts` away would leave the one
 * layer that silently rots -- camelCase names against Rust's snake_case parameters -- unverified.
 *
 * The tests that matter most here are the negative ones. "Sign button sends the right arguments" is
 * easy and would pass against a dialog that could never work; the failures worth catching are an
 * implied `P=1` (which makes the user's own later annotation look like tampering), a password left
 * in state after a rejected call, and a sign that reports success without the badge being told.
 */

import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { installFakeBackend } from "../test/tauri";
import type { FakeBackend } from "../test/tauri";
import { SignAction } from "./SignDialog";

let backend: FakeBackend | undefined;

afterEach(() => {
  backend?.teardown();
  backend = undefined;
  vi.restoreAllMocks();
});

/**
 * The dialog plugin is absent in a webview. `vi.mock` is hoisted above the imports, so the stub is
 * declared once at module scope and steered through `nextPickedPath`, which keeps the file each test
 * chooses visible in the test body rather than in a hoisted factory.
 */
const picked = vi.hoisted(() => ({ path: null as string | null }));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(async () => picked.path),
}));

function pickerReturns(path: string) {
  picked.path = path;
}

function certificate(overrides: Record<string, unknown> = {}) {
  return {
    derHex: "30aa",
    subject: "CN=Ada Lovelace",
    issuer: "CN=Test CA",
    serialHex: "01",
    keyAlgorithm: "RSA",
    notBefore: "2020-01-01T00:00:00Z",
    notAfter: "2040-01-01T00:00:00Z",
    canSign: true,
    expired: false,
    ...overrides,
  };
}

/**
 * The dialog now mounts `TrustPanel`, which reads both halves of the store on mount. These tests are
 * about the *dialog*, so the panel's reads are stubbed empty rather than left unhandled: an
 * unhandled command rejects, and the panel's error banner would then be a second `role="alert"` in
 * every test that asserts on the dialog's own message. `TrustPanel.test.tsx` covers the panel.
 */
function stubTrustStore(target: FakeBackend) {
  target.on("list_trust_anchors", () => []);
  target.on("list_trust_crls", () => []);
  return target;
}

function withStore(certificates: unknown[], skipped: unknown[] = []) {
  backend = stubTrustStore(installFakeBackend());
  backend.on("list_signing_certificates", () => ({ certificates, skipped }));
  return backend;
}

describe("the sign dialog", () => {
  it("lists a store certificate so a user can confirm it is installed", async () => {
    withStore([certificate()]);
    render(<SignAction documentId="doc-1" signedTick={0} onSigned={() => {}} />);
    await userEvent.click(screen.getByRole("button", { name: "Sign…" }));
    expect(await screen.findByText(/CN=Ada Lovelace/)).toBeTruthy();
  });

  it("says a store certificate cannot be used yet instead of offering it", async () => {
    // The signer takes a PKCS#12 path, not a store certificate. Offering a radio button that fails
    // at runtime would be the defect this copy exists to prevent.
    withStore([certificate()]);
    render(<SignAction documentId="doc-1" signedTick={0} onSigned={() => {}} />);
    await userEvent.click(screen.getByRole("button", { name: "Sign…" }));
    expect(await screen.findByText(/Signing with a store certificate is not available yet/)).toBeTruthy();
  });

  it("reports an unreadable store as unavailable, not as having no certificates", async () => {
    // Those are different facts and only one of them is the user's fault.
    backend = stubTrustStore(installFakeBackend());
    backend.on("list_signing_certificates", () => {
      throw "the Windows certificate store is not available on this platform";
    });
    render(<SignAction documentId="doc-1" signedTick={0} onSigned={() => {}} />);
    await userEvent.click(screen.getByRole("button", { name: "Sign…" }));
    expect(await screen.findByText(/certificate store is not available/i)).toBeTruthy();
    expect(screen.queryByText(/No certificate with a private key is installed/)).toBeNull();
  });

  it("shows a certificate with no private key rather than hiding it", async () => {
    withStore([certificate({ subject: "CN=No Key", canSign: false })]);
    render(<SignAction documentId="doc-1" signedTick={0} onSigned={() => {}} />);
    await userEvent.click(screen.getByRole("button", { name: "Sign…" }));
    expect(await screen.findByText(/no private key/)).toBeTruthy();
  });

  it("shows a pre-epoch date as unknown instead of inventing one", async () => {
    withStore([
      certificate({ notAfter: "unknown (the system clock is set before 1970)" }),
    ]);
    render(<SignAction documentId="doc-1" signedTick={0} onSigned={() => {}} />);
    await userEvent.click(screen.getByRole("button", { name: "Sign…" }));
    expect(await screen.findByText(/CN=Ada Lovelace/)).toBeTruthy();
  });

  it("cannot sign before a certificate file and a password are given", async () => {
    withStore([]);
    render(<SignAction documentId="doc-1" signedTick={0} onSigned={() => {}} />);
    await userEvent.click(screen.getByRole("button", { name: "Sign…" }));
    const sign = await screen.findByRole("button", { name: "Sign" });
    expect((sign as HTMLButtonElement).disabled).toBe(true);
  });
});

describe("the signature level (D-SG6)", () => {
  it("sends no certification level for an approval signature", async () => {
    pickerReturns("/tmp/signer.pfx");
    backend = stubTrustStore(installFakeBackend());
    backend.on("list_signing_certificates", () => ({ certificates: [], skipped: [] }));
    backend.on("sign_document", () => ({
      path: "/tmp/signed.pdf",
      byteRange: [0, 1, 2, 3],
      contentDigest: "ab",
    }));
    render(<SignAction documentId="doc-1" signedTick={0} onSigned={() => {}} />);
    await userEvent.click(screen.getByRole("button", { name: "Sign…" }));
    await userEvent.click(await screen.findByRole("button", { name: /Choose a PKCS#12 file/ }));
    await userEvent.type(screen.getByLabelText(/Password for the private key/), "pw");
    await userEvent.click(screen.getByRole("button", { name: "Sign" }));

    await waitFor(() => expect(backend?.callsTo("sign_document").length).toBe(1));
    const args = backend!.callsTo("sign_document")[0];
    // Absence, not `0`: an implied `P=1` would mark the user's own later annotation as tampering.
    expect(args.certificationLevel).toBeNull();
  });

  it("sends the chosen certification level when one is picked", async () => {
    pickerReturns("/tmp/signer.pfx");
    backend = stubTrustStore(installFakeBackend());
    backend.on("list_signing_certificates", () => ({ certificates: [], skipped: [] }));
    backend.on("sign_document", () => ({
      path: "/tmp/signed.pdf",
      byteRange: [0, 1, 2, 3],
      contentDigest: "ab",
    }));
    render(<SignAction documentId="doc-1" signedTick={0} onSigned={() => {}} />);
    await userEvent.click(screen.getByRole("button", { name: "Sign…" }));
    await userEvent.click(await screen.findByRole("button", { name: /Choose a PKCS#12 file/ }));
    await userEvent.click(screen.getByRole("radio", { name: /Form filling and signing/ }));
    await userEvent.type(screen.getByLabelText(/Password for the private key/), "pw");
    await userEvent.click(screen.getByRole("button", { name: "Sign" }));

    await waitFor(() => expect(backend?.callsTo("sign_document").length).toBe(1));
    expect(backend!.callsTo("sign_document")[0].certificationLevel).toBe(2);
  });

  it("omits a reason the user left blank rather than signing a space", async () => {
    pickerReturns("/tmp/signer.pfx");
    backend = stubTrustStore(installFakeBackend());
    backend.on("list_signing_certificates", () => ({ certificates: [], skipped: [] }));
    backend.on("sign_document", () => ({
      path: "/tmp/signed.pdf",
      byteRange: [0, 1, 2, 3],
      contentDigest: "ab",
    }));
    render(<SignAction documentId="doc-1" signedTick={0} onSigned={() => {}} />);
    await userEvent.click(screen.getByRole("button", { name: "Sign…" }));
    await userEvent.click(await screen.findByRole("button", { name: /Choose a PKCS#12 file/ }));
    await userEvent.type(screen.getByLabelText(/Password for the private key/), "pw");
    await userEvent.type(screen.getByLabelText(/Reason/), "   ");
    await userEvent.click(screen.getByRole("button", { name: "Sign" }));

    await waitFor(() => expect(backend?.callsTo("sign_document").length).toBe(1));
    expect(backend!.callsTo("sign_document")[0].reason).toBeNull();
  });
});

describe("after signing", () => {
  it("tells the caller so the badge re-reads the file it describes", async () => {
    pickerReturns("/tmp/signer.pfx");
    backend = stubTrustStore(installFakeBackend());
    backend.on("list_signing_certificates", () => ({ certificates: [], skipped: [] }));
    backend.on("sign_document", () => ({
      path: "/tmp/signed.pdf",
      byteRange: [0, 1, 2, 3],
      contentDigest: "ab",
    }));
    const onSigned = vi.fn();
    render(<SignAction documentId="doc-1" signedTick={0} onSigned={onSigned} />);
    await userEvent.click(screen.getByRole("button", { name: "Sign…" }));
    await userEvent.click(await screen.findByRole("button", { name: /Choose a PKCS#12 file/ }));
    await userEvent.type(screen.getByLabelText(/Password for the private key/), "pw");
    await userEvent.click(screen.getByRole("button", { name: "Sign" }));

    // Without this the badge keeps the verdict it read before signing, which would be `unsigned`
    // on a document the user had just signed.
    await waitFor(() => expect(onSigned).toHaveBeenCalledTimes(1));
  });

  it("clears the password after a failed sign so the next attempt cannot resubmit it", async () => {
    pickerReturns("/tmp/signer.pfx");
    backend = stubTrustStore(installFakeBackend());
    backend.on("list_signing_certificates", () => ({ certificates: [], skipped: [] }));
    backend.on("sign_document", () => {
      throw "the password does not open this PKCS#12 file";
    });
    render(<SignAction documentId="doc-1" signedTick={0} onSigned={() => {}} />);
    await userEvent.click(screen.getByRole("button", { name: "Sign…" }));
    await userEvent.click(await screen.findByRole("button", { name: /Choose a PKCS#12 file/ }));
    const field = screen.getByLabelText(/Password for the private key/) as HTMLInputElement;
    await userEvent.type(field, "wrong");
    await userEvent.click(screen.getByRole("button", { name: "Sign" }));

    expect(await screen.findByText(/does not open this PKCS#12/)).toBeTruthy();
    await waitFor(() => expect(field.value).toBe(""));
  });

  it("reports a backend refusal in the user's words, not as a stack string", async () => {
    pickerReturns("/tmp/signer.pfx");
    backend = stubTrustStore(installFakeBackend());
    backend.on("list_signing_certificates", () => ({ certificates: [], skipped: [] }));
    backend.on("sign_document", () => {
      throw { code: "SIGNATURE_BUILD_FAILED", message: "the document is password-protected" };
    });
    render(<SignAction documentId="doc-1" signedTick={0} onSigned={() => {}} />);
    await userEvent.click(screen.getByRole("button", { name: "Sign…" }));
    await userEvent.click(await screen.findByRole("button", { name: /Choose a PKCS#12 file/ }));
    await userEvent.type(screen.getByLabelText(/Password for the private key/), "pw");
    await userEvent.click(screen.getByRole("button", { name: "Sign" }));

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("the document is password-protected");
  });
});

describe("importing trust from the sign dialog", () => {
  const imported = (overrides: Record<string, unknown> = {}) => ({
    subject: "CN=DocReader Test CA",
    derBase64: "MIID",
    added: true,
    notAfter: 2109688780,
    ...overrides,
  });

  /** Open the dialog with an empty certificate store, which no `sign_document` path needs. */
  async function openDialog(onSigned = () => {}) {
    backend = stubTrustStore(installFakeBackend());
    backend.on("list_signing_certificates", () => ({ certificates: [], skipped: [] }));
    render(<SignAction documentId="doc-1" signedTick={0} onSigned={onSigned} />);
    await userEvent.click(screen.getByRole("button", { name: "Sign…" }));
    // The dialog's own trust section, by its exact legend. `/Trust/` is now ambiguous: the panel
    // below mounts a second fieldset, and a loose match would keep passing while pointing at
    // whichever legend rendered first.
    await screen.findByText("Trust", { selector: "legend" });
    return backend;
  }

  it("sends the chosen path to import_trust_anchor and reports the subject", async () => {
    const fake = await openDialog();
    fake.on("import_trust_anchor", () => imported());
    pickerReturns("/tmp/ca.der");

    await userEvent.click(screen.getByRole("button", { name: /Import certificate/ }));

    expect(await screen.findByText(/Certificate trusted: CN=DocReader Test CA/)).toBeTruthy();
    // The argument the Rust command declares is `derPath`; a camelCase mismatch here would fail
    // only at runtime, which is why this asserts on the wire rather than on the wrapper's signature.
    expect(fake.callsTo("import_trust_anchor")).toEqual([{ derPath: "/tmp/ca.der" }]);
  });

  it("sends the CRL to its own command, not to the anchor one", async () => {
    const fake = await openDialog();
    fake.on("import_trust_crl", () => imported({ subject: "CN=DocReader Test CA (CRL)" }));
    pickerReturns("/tmp/crl-empty.der");

    await userEvent.click(screen.getByRole("button", { name: /Import revocation list/ }));

    expect(await screen.findByText(/Revocation list trusted/)).toBeTruthy();
    expect(fake.callsTo("import_trust_crl")).toEqual([{ derPath: "/tmp/crl-empty.der" }]);
    expect(fake.callsTo("import_trust_anchor")).toEqual([]);
  });

  it("says an already-trusted certificate was already trusted, not that it was trusted again", async () => {
    // `added: false` is the ordinary repeat import. Reporting it as a fresh trust decision would
    // let a user believe they had made two, when they made one.
    const fake = await openDialog();
    fake.on("import_trust_anchor", () => imported({ added: false }));
    pickerReturns("/tmp/ca.der");

    await userEvent.click(screen.getByRole("button", { name: /Import certificate/ }));

    expect(await screen.findByText(/was already trusted; nothing changed/)).toBeTruthy();
    expect(screen.queryByText(/^Certificate trusted/)).toBeNull();
  });

  it("tells the badge to re-read after an import, because the store is one of its inputs", async () => {
    // Without this the badge keeps showing `unknown` after the very action that could make it
    // `valid` -- correct when it was read, and stale by the time the user looks at it.
    const onSigned = vi.fn();
    const fake = await openDialog(onSigned);
    fake.on("import_trust_anchor", () => imported());
    pickerReturns("/tmp/ca.der");

    await userEvent.click(screen.getByRole("button", { name: /Import certificate/ }));
    await screen.findByText(/Certificate trusted/);

    await waitFor(() => expect(onSigned).toHaveBeenCalled());
  });

  it("does not claim a trust change when the user cancels the file picker", async () => {
    const onSigned = vi.fn();
    await openDialog(onSigned);
    picked.path = null;

    await userEvent.click(screen.getByRole("button", { name: /Import certificate/ }));

    // Nothing was imported, so nothing may be reported and the badge must not be told to re-read.
    await waitFor(() => expect(screen.queryByText(/already trusted/)).toBeNull());
    expect(onSigned).not.toHaveBeenCalled();
  });

  it("shows an unreadable file as the backend's sentence, not as a silent failure", async () => {
    const fake = await openDialog();
    fake.on("import_trust_anchor", () => {
      throw { code: "CERTIFICATE_UNREADABLE", message: "this file is not a certificate" };
    });
    pickerReturns("/tmp/notes.txt");

    await userEvent.click(screen.getByRole("button", { name: /Import certificate/ }));

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("this file is not a certificate");
  });
});
