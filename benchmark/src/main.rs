/// Text Generation Inference benchmarking tool
///
/// Inspired by the great Oha app: https://github.com/hatoo/oha
/// and: https://github.com/orhun/rust-tui-template
use clap::Parser;
use std::path::Path;
use text_generation_client::ShardedClient;
use tokenizers::{FromPretrainedParameters, Tokenizer};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// App Configuration
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// The name of the tokenizer (as in model_id on the huggingface hub, or local path).
    #[clap(short, long, env)]
    tokenizer_name: String,

    /// The revision to use for the tokenizer if on the hub.
    #[clap(default_value = "main", long, env)]
    revision: String,

    /// The various batch sizes to benchmark for, the idea is to get enough
    /// batching to start seeing increased latency, this usually means you're
    /// moving from memory bound (usual as BS=1) to compute bound, and this is
    /// a sweet spot for the maximum batch size for the model under test
    #[clap(short, long)]
    batch_size: Option<Vec<u32>>,

    /// This is the initial prompt sent to the text-generation-server length
    /// in token. Longer prompt will slow down the benchmark. Usually the
    /// latency grows somewhat linearly with this for the prefill step.
    ///
    /// Most importantly, the prefill step is usually not the one dominating
    /// your runtime, so it's ok to keep it short.
    #[clap(default_value = "10", short, long, env)]
    sequence_length: u32,

    /// This is how many tokens will be generated by the server and averaged out
    /// to give the `decode` latency. This is the *critical* number you want to optimize for
    /// LLM spend most of their time doing decoding.
    ///
    /// Decode latency is usually quite stable.
    #[clap(default_value = "8", short, long, env)]
    decode_length: u32,

    ///How many runs should we average from
    #[clap(default_value = "10", short, long, env)]
    runs: usize,

    /// Number of warmup cycles
    #[clap(default_value = "1", short, long, env)]
    warmups: usize,

    /// The location of the grpc socket. This benchmark tool bypasses the router
    /// completely and directly talks to the gRPC processes
    #[clap(default_value = "/tmp/text-generation-server-0", short, long, env)]
    master_shard_uds_path: String,

    /// Generation parameter in case you want to specifically test/debug particular
    /// decoding strategies, for full doc refer to the `text-generation-server`
    #[clap(long, env)]
    temperature: Option<f32>,

    /// Generation parameter in case you want to specifically test/debug particular
    /// decoding strategies, for full doc refer to the `text-generation-server`
    #[clap(long, env)]
    top_k: Option<u32>,

    /// Generation parameter in case you want to specifically test/debug particular
    /// decoding strategies, for full doc refer to the `text-generation-server`
    #[clap(long, env)]
    top_p: Option<f32>,

    /// Generation parameter in case you want to specifically test/debug particular
    /// decoding strategies, for full doc refer to the `text-generation-server`
    #[clap(long, env)]
    typical_p: Option<f32>,

    /// Generation parameter in case you want to specifically test/debug particular
    /// decoding strategies, for full doc refer to the `text-generation-server`
    #[clap(long, env)]
    repetition_penalty: Option<f32>,

    /// Generation parameter in case you want to specifically test/debug particular
    /// decoding strategies, for full doc refer to the `text-generation-server`
    #[clap(long, env)]
    watermark: bool,

    /// Generation parameter in case you want to specifically test/debug particular
    /// decoding strategies, for full doc refer to the `text-generation-server`
    #[clap(long, env)]
    do_sample: bool,

    #[clap(long, env)]
    min_new_tokens: Option<u32>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logging();

    // Get args
    let args = Args::parse();
    // Pattern match configuration
    let Args {
        tokenizer_name,
        revision,
        batch_size,
        sequence_length,
        decode_length,
        runs,
        warmups,
        temperature,
        top_k,
        top_p,
        typical_p,
        repetition_penalty,
        watermark,
        do_sample,
        min_new_tokens,
        master_shard_uds_path,
    } = args;

    let batch_size = batch_size.unwrap_or(vec![1, 2, 4, 8, 16, 32]);

    // Tokenizer instance
    // This will only be used to validate payloads
    tracing::info!("Loading tokenizer");
    let local_path = Path::new(&tokenizer_name);
    let tokenizer =
        if local_path.exists() && local_path.is_dir() && local_path.join("tokenizer.json").exists()
        {
            // Load local tokenizer
            tracing::info!("Found local tokenizer");
            Tokenizer::from_file(local_path.join("tokenizer.json")).unwrap()
        } else {
            tracing::info!("Downloading tokenizer");

            // Parse Huggingface hub token
            let auth_token = std::env::var("HUGGING_FACE_HUB_TOKEN").ok();

            // Download and instantiate tokenizer
            // We need to download it outside of the Tokio runtime
            let params = FromPretrainedParameters {
                revision,
                auth_token,
                ..Default::default()
            };
            Tokenizer::from_pretrained(tokenizer_name.clone(), Some(params)).unwrap()
        };
    tracing::info!("Tokenizer loaded");

    // Launch Tokio runtime
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            // Instantiate sharded client from the master unix socket
            tracing::info!("Connect to model server");
            let mut sharded_client = ShardedClient::connect_uds(master_shard_uds_path)
                .await
                .expect("Could not connect to server");
            // Clear the cache; useful if the webserver rebooted
            sharded_client
                .clear_cache(None)
                .await
                .expect("Unable to clear cache");
            tracing::info!("Connected");

            // Run app
            text_generation_benchmark::run(
                tokenizer_name,
                tokenizer,
                batch_size,
                sequence_length,
                decode_length,
                runs,
                warmups,
                temperature,
                top_k,
                top_p,
                typical_p,
                repetition_penalty,
                watermark,
                do_sample,
                min_new_tokens,
                sharded_client,
            )
            .await
            .unwrap();
        });
    Ok(())
}

/// Init logging using LOG_LEVEL
fn init_logging() {
    // STDOUT/STDERR layer
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_file(true)
        .with_line_number(true);

    // Filter events with LOG_LEVEL
    let env_filter =
        EnvFilter::try_from_env("LOG_LEVEL").unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer)
        .init();
}
