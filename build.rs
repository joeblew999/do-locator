fn main() {
    connectrpc_build::Config::new()
        .files(&["proto/locator/v1/locator.proto"])
        .includes(&["proto"])
        .include_file("_connectrpc.rs")
        .compile()
        .expect("failed to compile protos");
}
