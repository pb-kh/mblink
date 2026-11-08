fn main() {
    tonic_prost_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_protos(
            &["../../../proto/mblink/link/v1/link.proto"],
            &["../../../proto"],
        )
        .expect("failed to compile link.proto");
}
