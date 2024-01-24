use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;

use crate::sound_flow::sound_flow_client::SoundFlowClient;

pub mod sound_flow {
    tonic::include_proto!("sound_flow");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = SoundFlowClient::connect("http://[::1]:50051").await?;
    let (tx, rx) = tokio::sync::mpsc::channel(128);

    println!("*** SIMPLE FEEDBACK ***");
    let response = client
        .get_flow(()).await?;
    let mut flow = response.into_inner();
    tokio::spawn(async move {
        loop {
            if let Some(Ok(value)) = flow.next().await {
                tx.send(value).await.unwrap();
            }
        }
    });
    client.send_flow(ReceiverStream::new(rx)).await?;
    loop {}
    // Ok(())
}