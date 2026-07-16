use forge::bus::Invocation;
use forge::kernel::{Kernel, KernelConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let kernel = Kernel::start(KernelConfig::default());

    kernel
        .bus()
        .register_handler("ping", |_inv: Invocation| async move {
            Ok(bytes::Bytes::from_static(b"pong"))
        })
        .await;

    let result = kernel
        .bus()
        .dispatch(Invocation::simple("ping", vec![]))
        .await?;

    assert_eq!(&result[..], b"pong");
    println!("ping → {}", String::from_utf8_lossy(&result));
    Ok(())
}
