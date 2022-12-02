use hkdf::Hkdf;
use sha2::Sha256;

// this is the most common way to use HKDF: you provide the Initial Key
// Material and an optional salt, then you expand it (perhaps multiple times)
// into some Output Key Material bound to an "info" context string.

fn main() {
    let ikm = hex::decode("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b").unwrap();
    let salt = hex::decode("000102030405060708090a0b0c").unwrap();
    let info = hex::decode("f0f1f2f3f4f5f6f7f8f9").unwrap();

    let hk = Hkdf::<Sha256>::new(Some(&salt[..]), &ikm);
    let mut okm = [0u8; 42];
    hk.expand(&info, &mut okm)
        .expect("42 is a valid length for Sha256 to output");

    println!("Vector 1 OKM is {}", hex::encode(&okm[..]));
    println!("Matched with https://tools.ietf.org/html/rfc5869#appendix-A.1");

    let expected =
        "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865";
    assert_eq!(hex::encode(&okm[..]), expected);
}
