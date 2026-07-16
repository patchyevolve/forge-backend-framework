fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    // SAFETY: build script runs single-threaded before any other code
    unsafe {
        std::env::set_var("PROTOC", protoc);
    }
    tonic_build::compile_protos("proto/forge_plugin_v1.proto")?;
    Ok(())
}
