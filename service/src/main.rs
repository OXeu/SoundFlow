use std::mem::MaybeUninit;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::Stream;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use pulsectl::controllers::{DeviceControl, SinkController};
use ringbuf::{Consumer, HeapRb, Producer, SharedRb};
use tokio::sync::broadcast::{channel, Sender};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tonic::{Request, Response, Status, Streaming};
use tonic::codegen::CompressionEncoding;
use tonic::transport::Server;

use crate::sound_flow::{Device, DeviceId, Devices, Direction, Flow};
use crate::sound_flow::sound_flow_server::{SoundFlow, SoundFlowServer};

pub mod sound_flow {
    tonic::include_proto!("sound_flow");
}

struct SoundFlowService {
    consumer: Sender<Result<Flow, ()>>,
    producer: Arc<Mutex<Producer<Vec<f32>, Arc<SharedRb<Vec<f32>, Vec<MaybeUninit<Vec<f32>>>>>>>>,
}
const PACKAGE_SIZE: usize = 1000; // per package will send data like: [f32;PACKAGE_SIZE], not too small to avoid overhead.

#[tonic::async_trait]
impl SoundFlow for SoundFlowService {
    async fn get_devices(&self, _request: Request<Direction>) -> Result<Response<Devices>, Status> {
        let mut handler = SinkController::create().unwrap();
        let devices = handler.list_devices().unwrap();
        let devices = Devices {
            devices: devices.iter().map(|device| {
                println!("Device: {:?}", device);
                Device {
                    id: device.index,
                    name: device.description.clone().unwrap_or_else(|| "Unknown".to_string()),
                }
            }).collect()
        };
        Ok(Response::new(devices))
    }

    async fn send_flow(&self, request: Request<Streaming<Flow>>) -> Result<Response<()>, Status> {
        let mut stream = request.into_inner();
        let producer = self.producer.clone();
        tokio::spawn(async move {
            while let Some(flow) = stream.next().await {
                if let Ok(flow) = flow {
                    if producer.lock().unwrap().push(flow.flow).is_err() {
                        eprintln!("input stream fell behind: try increasing latency");
                    }
                }
            }
        });
        Ok(Response::new(()))
    }

    type GetFlowStream = ReceiverStream<Result<Flow, Status>>;

    async fn get_flow(&self, _request: Request<()>) -> Result<Response<Self::GetFlowStream>, Status> {
        let mut consumer = self.consumer.subscribe();
        let (tx, rx) = tokio::sync::mpsc::channel(128);
        tokio::spawn(async move {
            loop {
                if let Ok(Ok(v)) = consumer.recv().await {
                    let _ = tx.send(Ok(v)).await;
                };
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn set_device(&self, request: Request<DeviceId>) -> Result<Response<()>, Status> {
        let id = request.into_inner().id;
        let mut handler = SinkController::create().unwrap();
        let devices = handler.list_devices().unwrap();
        let device = devices.iter().find(|device| device.index == id).ok_or_else(|| Status::not_found("Device not found"))?;
        handler.set_default_device(&*device.name.clone().unwrap()).unwrap();
        Ok(Response::new(()))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (mut recorded_consumer, _input_stream) = microphone();
    let (output_producer, _output_stream) = speaker();
    let (tx, _) = channel(128);
    let addr = "[::1]:50051".parse().unwrap();
    let service = SoundFlowService {
        consumer: tx.clone(),
        producer: Arc::new(Mutex::new(output_producer)),
    };

    println!("Sound Flow Server listening on {}", addr);

    let service = SoundFlowServer::new(service)
        .send_compressed(CompressionEncoding::Gzip)
        .accept_compressed(CompressionEncoding::Gzip);

    tokio::spawn(async move {
        let _ = Server::builder().add_service(service).serve(addr).await;
    });
    loop {
        if let Some(v) = recorded_consumer.pop() {
            let _ = tx.send(Ok(Flow {
                flow: v
            }));
        } else {
            tokio::time::sleep(Duration::from_millis(10)).await
        };
    }
}

fn err_fn(err: cpal::StreamError) {
    eprintln!("an error occurred on stream: {}", err);
}

fn microphone() -> (Consumer<Vec<f32>, Arc<SharedRb<Vec<f32>, Vec<MaybeUninit<Vec<f32>>>>>>, Stream) {
    let host = cpal::default_host();
    // Find devices.
    let input_device = host.default_input_device().expect("failed to find input device");
    println!("Using input device: \"{}\"", input_device.name().unwrap());
    let config: cpal::StreamConfig = input_device.default_input_config().unwrap().into();
    // The buffer to share samples
    let ring = HeapRb::<Vec<f32>>::new(128);
    let (mut producer, consumer) = ring.split();


    let input_data_fn = move |data: &[f32], _: &cpal::InputCallbackInfo| {
        data.chunks(PACKAGE_SIZE).for_each(|chunk| {
            if producer.push(chunk.to_vec()).is_err() {
                eprintln!("input stream fell behind: try increasing latency");
            }
        });
    };

    let input_stream = input_device.build_input_stream(&config, input_data_fn, err_fn, None).unwrap();
    input_stream.play().unwrap();
    return (consumer, input_stream);
}

fn speaker() -> (Producer<Vec<f32>, Arc<SharedRb<Vec<f32>, Vec<MaybeUninit<Vec<f32>>>>>>, Stream) {
    let host = cpal::default_host();
    // Find devices.
    let output_device =
        host.default_output_device()
            .expect("failed to find output device");
    println!("Using output device: \"{}\"", output_device.name().unwrap());
    let config: cpal::StreamConfig = output_device.default_input_config().unwrap().into();
    // The buffer to share samples
    let ring = HeapRb::<Vec<f32>>::new(128);
    let (producer, mut consumer) = ring.split();

    // Fill the samples with 0.0 equal to the length of the delay.
    let output_data_fn = move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
        for sample in data.chunks_mut(PACKAGE_SIZE) {
            if let Some(consumer_data) = consumer.pop() {
                let min = sample.len().min(consumer_data.len());
                sample[..min].copy_from_slice(&consumer_data.as_slice()[..min]);
            } else {
                sample.iter_mut().for_each(|x| *x = 0.0);
            }
        }

    };
    let output_stream = output_device.build_output_stream(&config, output_data_fn, err_fn, None).unwrap();
    output_stream.play().unwrap();
    return (producer, output_stream);
}