//! The signature check on Bitcoin Core's SHA256SUMS: gpg's status lines counted
//! by the rules.

use spi::signature::count;

fn signers() -> Vec<(String, String)> {
    ["A", "B", "C"]
        .map(|n| (n.repeat(40), n.to_lowercase()))
        .into()
}

/// One signature as gpg reports it: of `kind`, by the key whose primary
/// fingerprint is `primary` (a letter, repeated).
fn sig(kind: &str, primary: &str) -> String {
    let f = primary.repeat(40);
    format!("[GNUPG:] NEWSIG\n[GNUPG:] {kind} K u\n[GNUPG:] VALIDSIG {f} 1 2 3 4 5 6 7 8 {f}\n")
}

#[test]
fn any_bad_signature_fails_whoever_else_signed() {
    let bad = sig("GOODSIG", "A") + "[GNUPG:] BADSIG K9 mallory\n";
    assert!(
        count(&bad, &signers())
            .unwrap_err()
            .contains("BAD signature")
    );
}

#[test]
fn only_good_trusted_signatures_count_and_each_builder_counts_once() {
    let unknown = "[GNUPG:] ERRSIG K3 1 8 00 1 9\n[GNUPG:] NO_PUBKEY K3\n";
    let status = sig("GOODSIG", "A") + &sig("GOODSIG", "A") + &sig("GOODSIG", "D");
    let status = status + &sig("REVKEYSIG", "B") + unknown;
    assert_eq!(count(&status, &signers()).unwrap(), ["a"]);
}

#[test]
fn a_signature_by_a_key_that_has_since_expired_still_counts() {
    let status = sig("GOODSIG", "A") + &sig("EXPKEYSIG", "B") + "[GNUPG:] KEYEXPIRED 1\n";
    assert_eq!(count(&status, &signers()).unwrap(), ["a", "b"]);
}
