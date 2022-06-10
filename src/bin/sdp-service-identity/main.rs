#[derive(Debug)]
struct SDPIdentity {
    name: Option<String>,
    device_id: Option<String>,
}

#[derive(Debug)]
struct SDPService {
    name: String,
    namespace: String,
    identity: SDPIdentity,
}


fn main() {
    println!("Hello, world!");
}
