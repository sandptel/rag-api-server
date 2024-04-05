mod backend;
mod error;
mod utils;

use anyhow::Result;
use chat_prompts::PromptTemplateType;
use clap::{crate_version, Arg, ArgAction, Command};
use error::ServerError;
use hyper::{
    header,
    service::{make_service_fn, service_fn},
    Body, Request, Response, Server, StatusCode,
};
use llama_core::{Metadata, ModelInfo};
use once_cell::sync::OnceCell;
use std::{net::SocketAddr, path::PathBuf, str::FromStr};
use utils::{is_valid_url, print_log_begin_separator, print_log_end_separator};

type Error = Box<dyn std::error::Error + Send + Sync + 'static>;

// default socket address
const DEFAULT_SOCKET_ADDRESS: &str = "0.0.0.0:8080";
// Qdrant config
pub(crate) static QDRANT_CONFIG: OnceCell<QdrantConfig> = OnceCell::new();
// global system prompt
pub(crate) static GLOBAL_SYSTEM_PROMPT: OnceCell<String> = OnceCell::new();

#[derive(Clone, Debug)]
pub struct AppState {
    pub state_thing: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), ServerError> {
    let matches = Command::new("rag-api-server")
        .version(crate_version!())
        .arg(
            Arg::new("socket_addr")
                .short('s')
                .long("socket-addr")
                .value_name("IP:PORT")
                .help("Sets the socket address")
                .default_value(DEFAULT_SOCKET_ADDRESS),
        )
        .arg(
            Arg::new("model_name")
                .short('m')
                .long("model-name")
                .value_name("MODEL-NAME")
                .value_delimiter(',')
                .help("Sets names for chat and embedding models. The names are separated by comma without space, for example, 'Llama-2-7b,all-minilm'.")
        )
        .arg(
            Arg::new("model_alias")
                .short('a')
                .long("model-alias")
                .value_name("MODEL-ALIAS")
                .value_delimiter(',')
                .help("Sets model aliases")
                .default_value("default,embedding"),
        )
        .arg(
            Arg::new("ctx_size")
                .short('c')
                .long("ctx-size")
                .value_name("CTX_SIZE")
                .value_delimiter(',')
                .value_parser(clap::value_parser!(u64))
                .help("Sets context sizes for chat and embedding models. The sizes are separated by comma without space, for example, '4096,384'. The first value is for the chat model, and the second value is for the embedding model.")
                .default_value("4096,384"),
        )
        .arg(
            Arg::new("reverse_prompt")
                .short('r')
                .long("reverse-prompt")
                .value_name("REVERSE_PROMPT")
                .help("Halt generation at PROMPT, return control."),
        )
        .arg(
            Arg::new("prompt_template")
                .short('p')
                .long("prompt-template")
                .value_parser([
                    "llama-2-chat",
                    "codellama-instruct",
                    "codellama-super-instruct",
                    "mistral-instruct",
                    "mistrallite",
                    "openchat",
                    "human-assistant",
                    "vicuna-1.0-chat",
                    "vicuna-1.1-chat",
                    "vicuna-llava",
                    "chatml",
                    "baichuan-2",
                    "wizard-coder",
                    "zephyr",
                    "stablelm-zephyr",
                    "intel-neural",
                    "deepseek-chat",
                    "deepseek-coder",
                    "solar-instruct",
                    "gemma-instruct",
                ])
                .value_name("TEMPLATE")
                .help("Sets the prompt template.")
                .required(true)
        )
        .arg(
            Arg::new("system_prompt")
                .long("system-prompt")
                .help("Sets global system prompt.")
                .default_value(""),
        )
        .arg(
            Arg::new("qdrant_url")
                .long("qdrant-url")
                .help("Sets the url of Qdrant REST Service.")
                .default_value("http://localhost:6333"),
        )
        .arg(
            Arg::new("qdrant_collection_name")
                .long("qdrant-collection-name")
                .help("Sets the collection name of Qdrant.")
                .default_value("default"),
        )
        .arg(
            Arg::new("qdrant_limit")
                .long("qdrant-limit")
                .value_parser(clap::value_parser!(u64))
                .help("Max number of retrieved result.")
                .default_value("3"),
        )
        .arg(
            Arg::new("qdrant_score_threshold")
                .long("qdrant-score-threshold")
                .value_parser(clap::value_parser!(f32))
                .help("Minimal score threshold for the search result")
                .default_value("0.4"),
        )
        .arg(
            Arg::new("log_prompts")
                .long("log-prompts")
                .value_name("LOG_PROMPTS")
                .help("Print prompt strings to stdout")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("log_stat")
                .long("log-stat")
                .value_name("LOG_STAT")
                .help("Print statistics to stdout")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("log_all")
                .long("log-all")
                .value_name("LOG_all")
                .help("Print all log information to stdout")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("web_ui")
                .long("web-ui")
                .value_name("WEB_UI")
                .help("Root path for the Web UI files")
                .default_value("chatbot-ui"),
        )
        .get_matches();

    println!("\n[+] Configuring LlamaEdge RAG API server ...");

    // print the version of the server
    println!(
        "    * LlamaEdge RAG API server version: {}",
        env!("CARGO_PKG_VERSION"),
    );

    // socket address
    let socket_addr =
        matches
            .get_one::<String>("socket_addr")
            .ok_or(ServerError::ArgumentError(
                "Failed to parse the value of `socket_addr` CLI option".to_owned(),
            ))?;
    let addr = socket_addr
        .parse::<SocketAddr>()
        .map_err(|e| ServerError::SocketAddr(e.to_string()))?;
    println!(
        "    * Socket address: {socket_addr}",
        socket_addr = socket_addr
    );

    // model names
    let model_names: Vec<String> = matches
        .get_many::<String>("model_name")
        .ok_or(ServerError::ArgumentError(
            "Failed to parse the value of `--model-names` CLI option".to_owned(),
        ))?
        .map(|s| s.to_string())
        .collect();
    if model_names.len() != 2 {
        return Err(ServerError::ArgumentError(
            "LlamaEdge RAG API server requires a chat model and an embedding model.".to_owned(),
        ));
    }
    println!("    * Model names: {names}", names = model_names.join(","));

    // model aliases
    let model_aliases: Vec<String> = matches
        .get_many::<String>("model_alias")
        .ok_or(ServerError::ArgumentError(
            "Failed to parse the value of `--model-alias` CLI option".to_owned(),
        ))?
        .map(|s| s.to_string())
        .collect();
    if model_aliases.len() != 2 {
        return Err(ServerError::ArgumentError(
            "LlamaEdge RAG API server requires two model aliases: one for chat model, one for embedding model.".to_owned(),
        ));
    }
    println!(
        "    * Model aliases: {aliases}",
        aliases = model_aliases.join(",")
    );

    // create an `Options` instance
    let mut options = Metadata::default();

    // context sizes
    let ctx_sizes = matches
        .get_many::<u64>("ctx_size")
        .ok_or(ServerError::ArgumentError(
            "Failed to parse the value of `ctx_size` CLI option".to_owned(),
        ))?
        .map(|n| n.to_owned())
        .collect::<Vec<u64>>();
    if ctx_sizes.len() != 2 {
        return Err(ServerError::ArgumentError(
            "LlamaEdge RAG API server requires two context sizes: one for chat model, one for embedding model.".to_owned(),
        ));
    }
    let ctx_sizes_str: String = ctx_sizes
        .iter()
        .map(|n| n.to_string())
        .collect::<Vec<String>>()
        .join(",");
    println!("    * context sizes: {ctx_sizes_str}");

    // type of prompt template
    let prompt_template =
        matches
            .get_one::<String>("prompt_template")
            .ok_or(ServerError::ArgumentError(
                "The `prompt_template` CLI option is required".to_owned(),
            ))?;
    let template_ty = PromptTemplateType::from_str(prompt_template)
        .map_err(|e| ServerError::InvalidPromptTemplateType(e.to_string()))?;
    println!("    * Prompt template: {ty:?}", ty = &template_ty);
    let ref_template_ty = std::sync::Arc::new(template_ty);

    // reverse_prompt
    if let Some(reverse_prompt) = matches.get_one::<String>("reverse_prompt") {
        println!("    * Reverse prompt: {prompt}", prompt = &reverse_prompt);
        options.reverse_prompt = Some(reverse_prompt.to_string());
    }

    // global system prompt
    let global_system_prompt =
        matches
            .get_one::<String>("system_prompt")
            .ok_or(ServerError::ArgumentError(
                "Failed to parse the value of `system_prompt` CLI option".to_owned(),
            ))?;
    println!("    * Global system prompt: {}", global_system_prompt);
    GLOBAL_SYSTEM_PROMPT
        .set(global_system_prompt.to_owned())
        .map_err(|_| ServerError::Operation("Failed to set `GLOBAL_SYSTEM_PROMPT`.".to_string()))?;

    // qdrant config
    {
        // qdrant url
        let qdrant_url =
            matches
                .get_one::<String>("qdrant_url")
                .ok_or(ServerError::ArgumentError(
                    "Failed to parse the value of `qdrant_url` CLI option".to_owned(),
                ))?;

        if !is_valid_url(qdrant_url) {
            return Err(ServerError::ArgumentError(format!(
                "The URL of Qdrant REST API is invalid: {}.",
                qdrant_url
            )));
        }

        println!(
            "    * Qdrant server address: {socket_addr}",
            socket_addr = qdrant_url
        );

        // qdrant collection name
        let qdrant_collection_name = matches.get_one::<String>("qdrant_collection_name").ok_or(
            ServerError::ArgumentError(
                "Failed to parse the value of `qdrant_collection_name` CLI option".to_owned(),
            ),
        )?;
        println!(
            "    * Qdrant collection name: {name}",
            name = &qdrant_collection_name
        );

        // qdrant limit
        let qdrant_limit =
            matches
                .get_one::<u64>("qdrant_limit")
                .ok_or(ServerError::ArgumentError(
                    "Failed to parse the value of `qdrant_limit` CLI option".to_owned(),
                ))?;
        println!(
            "    * Max number of retrieved result: {limit}",
            limit = qdrant_limit
        );

        // qdrant score threshold
        let qdrant_score_threshold =
            matches
                .get_one::<f32>("qdrant_score_threshold")
                .ok_or(ServerError::ArgumentError(
                    "Failed to parse the value of `qdrant_score_threshold` CLI option".to_owned(),
                ))?;
        println!(
            "    * Qdrant score threshold: {threshold}",
            threshold = qdrant_score_threshold
        );

        let qdrant_config = QdrantConfig {
            url: qdrant_url.to_owned(),
            collection_name: qdrant_collection_name.to_owned(),
            limit: *qdrant_limit,
            score_threshold: *qdrant_score_threshold,
        };

        QDRANT_CONFIG
            .set(qdrant_config)
            .map_err(|_| ServerError::Operation("Failed to set '`QDRANT_CONFIG`.".to_string()))?;
    }

    // log prompts
    let log_prompts = matches.get_flag("log_prompts");
    println!("    * Log prompts: {enable}", enable = log_prompts);
    options.log_prompts = log_prompts;
    let ref_log_prompts = std::sync::Arc::new(log_prompts);

    // log statistics
    let log_stat = matches.get_flag("log_stat");
    println!("    * Log statistics: {enable}", enable = log_stat);

    // log all
    let log_all = matches.get_flag("log_all");
    println!("    * Log all information: {enable}", enable = log_all);

    // set `log_enable`
    if log_stat || log_all {
        options.log_enable = true;
    }

    println!("\n[+] Starting LlamaEdge RAG API server ...");

    if log_stat || log_all {
        print_log_begin_separator(
            "MODEL INFO (Load Model & Init Execution Context)",
            Some("*"),
            None,
        );
    }

    // * initialize the core context
    {
        // specify chat model
        let model_alias = model_aliases
            .first()
            .expect("Failed to get the model alias")
            .trim();
        let model_name = model_names
            .first()
            .expect("Failed to get the model name")
            .trim();
        let ctx_size = ctx_sizes.first().expect("Failed to get the context size");
        let mut metadata = options.clone();
        metadata.ctx_size = *ctx_size;

        let chat_models = vec![ModelInfo {
            model_name: model_name.to_string(),
            model_alias: model_alias.to_string(),
            metadata,
        }];

        // specify embedding model
        let model_alias = model_aliases
            .get(1)
            .expect("Failed to get the model alias")
            .trim();
        let model_name = model_names
            .get(1)
            .expect("Failed to get the model name")
            .trim();
        let ctx_size = ctx_sizes.get(1).expect("Failed to get the context size");
        let mut metadata = options.clone();
        metadata.ctx_size = *ctx_size;

        let embedding_models = vec![ModelInfo {
            model_name: model_name.to_string(),
            model_alias: model_alias.to_string(),
            metadata,
        }];

        // initialize the core context
        llama_core::init_core_context(&chat_models, Some(&embedding_models)).map_err(|e| {
            ServerError::Operation(format!("Failed to initialize the core context. {}", e))
        })?;

        // print plugin version info
        let plugin_info =
            llama_core::get_plugin_info().map_err(|e| ServerError::Operation(e.to_string()))?;
        println!(
            "    * wasi_nn-ggml plugin version: b{build_number} (commit {commit_id})",
            build_number = plugin_info.build_number,
            commit_id = plugin_info.commit_id,
        );
    }

    if log_stat || log_all {
        print_log_end_separator(Some("*"), None);
    }

    let new_service = make_service_fn(move |_| {
        let prompt_template_ty = ref_template_ty.clone();
        let log_prompts = ref_log_prompts.clone();
        let web_ui = matches
            .get_one::<String>("web_ui")
            .unwrap_or(&"chatbot-ui".to_owned())
            .to_string();
        async move {
            Ok::<_, Error>(service_fn(move |req| {
                handle_request(
                    req,
                    *prompt_template_ty.clone(),
                    *log_prompts.clone(),
                    web_ui.clone(),
                )
            }))
        }
    });

    let server = Server::bind(&addr).serve(new_service);

    println!("\n[+] Listening on http://{}", addr);

    match server.await {
        Ok(_) => Ok(()),
        Err(e) => Err(ServerError::Operation(e.to_string())),
    }
}

async fn handle_request(
    req: Request<Body>,
    template_ty: PromptTemplateType,
    log_prompts: bool,
    web_ui: String,
) -> Result<Response<Body>, hyper::Error> {
    let path_str = req.uri().path();
    let path_buf = PathBuf::from(path_str);
    let mut path_iter = path_buf.iter();
    path_iter.next(); // Must be Some(OsStr::new(&path::MAIN_SEPARATOR.to_string()))
    let root_path = path_iter.next().unwrap_or_default();
    let root_path = "/".to_owned() + root_path.to_str().unwrap_or_default();

    match root_path.as_str() {
        "/echo" => Ok(Response::new(Body::from("echo test"))),
        "/v1" => backend::handle_llama_request(req, template_ty, log_prompts).await,
        _ => Ok(static_response(path_str, web_ui)),
    }
}

fn static_response(path_str: &str, root: String) -> Response<Body> {
    let path = match path_str {
        "/" => "/index.html",
        _ => path_str,
    };

    let mime = mime_guess::from_path(path);

    match std::fs::read(format!("{root}/{path}")) {
        Ok(content) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime.first_or_text_plain().to_string())
            .body(Body::from(content))
            .unwrap(),
        Err(_) => {
            let body = Body::from(std::fs::read(format!("{root}/404.html")).unwrap_or_default());
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .header(header::CONTENT_TYPE, "text/html")
                .body(body)
                .unwrap()
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct QdrantConfig {
    pub(crate) url: String,
    pub(crate) collection_name: String,
    pub(crate) limit: u64,
    pub(crate) score_threshold: f32,
}
