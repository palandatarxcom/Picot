// ABOUTME: One-shot tweetnacl vector generator for relay E2EE interop tests.
// ABOUTME: Run with bun from an isolated dir (see README note); paste literals into
//
//   src-tauri/crates/picot-host/src/relay/e2ee.rs
//
// Why a script (spec 2026-10-17-relay-phase2 §2.4, R8): vectors must come from
// Paseo's exact library (npm tweetnacl), not from the Rust crypto_box stack
// under test. Same-library round-trips do not count as independent verification.
//
// Regenerate (never pollutes the repo):
//   mkdir -p /tmp/picot-nacl-vectors && cd /tmp/picot-nacl-vectors
//   echo '{"name":"picot-nacl-vectors","private":true}' > package.json
//   bun add tweetnacl        # resolves ^1.0.3, same family as Paseo packages/relay
//   cp /path/to/repo/scripts/relay-e2ee-tweetnacl-vectors.js .
//   bun relay-e2ee-tweetnacl-vectors.js

import { fromByteArray, toByteArray } from "base64-js";
import nacl from "tweetnacl";

const hex = (bytes) => Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");

// Fixed inputs (arbitrary but pinned literals).
const ourSecret = toByteArray("5oLZXkd0WpLm1XRpbhGZNvAGGtq7m1jH4FqIw0GxP2k=");
const peerSecret = toByteArray("mUbxT9HqGHGf6FUYkq+QOcE4COrCRcblDHTkfk/2rW4=");
const nonce = toByteArray("3J98TtXGDGfFQ0YCFfOCBgWts5XaPo1b");
const plaintext = new TextEncoder().encode("picot relay e2ee tweetnacl interop vector");

const peerKp = nacl.box.keyPair.fromSecretKey(peerSecret);

// Shared key = nacl.box.before (what the daemon derives via X25519+HSalsa20).
const shared = nacl.box.before(peerKp.publicKey, ourSecret);

// box.after with the precomputed key — what seal_with_nonce must reproduce byte-exact.
const ct = nacl.box.after(plaintext, nonce, shared);
const bundle = new Uint8Array(nonce.length + ct.length);
bundle.set(nonce, 0);
bundle.set(ct, nonce.length);

// Construction sanity: the full nacl.box path must equal the after-path we mirror.
const direct = nacl.box(plaintext, nonce, peerKp.publicKey, ourSecret);
if (Buffer.compare(Buffer.from(direct), Buffer.from(ct)) !== 0) {
  throw new Error("nacl.box != nacl.box.after — construction mismatch");
}

// Round-trip sanity through the reference library itself.
const opened = nacl.box.open.after(ct, nonce, shared);
if (Buffer.compare(Buffer.from(opened), Buffer.from(plaintext)) !== 0) {
  throw new Error("tweetnacl self round-trip failed");
}

console.log("// paste into src-tauri/crates/picot-host/src/relay/e2ee.rs tests:");
console.log(`const VECTOR_OUR_SECRET_HEX: &str = "${hex(ourSecret)}";`);
console.log(`const VECTOR_PEER_SECRET_HEX: &str = "${hex(peerSecret)}";`);
console.log(`const VECTOR_PEER_PUBLIC_B64: &str = "${fromByteArray(peerKp.publicKey)}";`);
console.log(`const VECTOR_NONCE_HEX: &str = "${hex(nonce)}";`);
console.log(
  `const VECTOR_PLAINTEXT: &[u8] = b"${Array.from(plaintext, (b) => String.fromCharCode(b)).join("")}";`,
);
console.log(`const VECTOR_SHARED_HEX: &str = "${hex(shared)}";`);
console.log(`const VECTOR_BUNDLE_HEX: &str = "${hex(bundle)}";`);
