use std::{fs::File, io::Write};

fn main() {
    let subject_alt_names = vec!["localhost".to_string()];
    let cert = rcgen::generate_simple_self_signed(subject_alt_names).unwrap();

    let mut file = File::create("cert.pem").unwrap();
    file.write_all(cert.cert.pem().as_bytes()).unwrap();
    file.flush().unwrap();

    let mut file = File::create("key.pem").unwrap();
    file.write_all(cert.signing_key.serialize_pem().as_bytes())
        .unwrap();
}
