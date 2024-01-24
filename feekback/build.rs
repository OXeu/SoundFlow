fn main() {
    tonic_build::compile_protos("../proto/sound_flow.proto")
        .unwrap_or_else(|e| panic!("Failed to compile protos {:?}", e));
}