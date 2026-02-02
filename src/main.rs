use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncBufReadExt;

// Constants & Structs

const GROQ_API_URL: &str = "https://api.groq.com/openai/v1/chat/completions";

const MODELS: &[&str] = &[
    "openai/gpt-oss-120b",
    "meta-llama/llama-4-maverick-17b-128e-instruct",
    "moonshotai/kimi-k2-instruct-0905",
];

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: Message,
}

#[derive(Debug, Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    delta: Delta,
    #[serde(default)]
    #[allow(dead_code)] // Fixes "field is never read" warning
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Delta {
    #[serde(default)]
    content: String,
}

// Helper Functions

fn get_config_path() -> PathBuf {
    let mut current_dir = std::env::current_dir().expect("Could not get current directory");
    current_dir.push(".env");
    current_dir
}

fn load_api_key() -> Result<String, String> {
    if let Ok(key) = std::env::var("GROQ_API_KEY") {
        return Ok(key);
    }

    Err("GROQ_API_KEY not found in .env file.".to_string())
}

fn save_api_key(key: &str) -> Result<(), String> {
    let path = get_config_path();
    let env_content = format!("GROQ_API_KEY={}", key.trim());
    fs::write(&path, env_content).map_err(|e| format!("Failed to write API key: {}", e))
}

async fn chat_completion(
    api_key: &str,
    model: &str,
    messages: &[Message],
    stream: bool,
) -> Result<String, reqwest::Error> {
    let client = reqwest::Client::new();

    if stream {
        let request = ChatRequest {
            model: model.to_string(),
            messages: messages.to_vec(),
            stream: true,
        };

        let mut response = client
            .post(GROQ_API_URL)
            .header(AUTHORIZATION, format!("Bearer {}", api_key))
            .header(CONTENT_TYPE, "application/json")
            .json(&request)
            .send()
            .await?;

        let mut full_response = String::new();

        while let Some(chunk) = response.chunk().await? {
            let chunk_str = String::from_utf8_lossy(&chunk);
            for line in chunk_str.lines() {
                let line = line.strip_prefix("data: ").unwrap_or(line);
                if line.is_empty() || line == "[DONE]" {
                    continue;
                }
                if let Ok(chunk_data) = serde_json::from_str::<StreamChunk>(line) {
                    if let Some(choice) = chunk_data.choices.first() {
                        if !choice.delta.content.is_empty() {
                            print!("{}", choice.delta.content);
                            io::stdout().flush().unwrap();
                            full_response.push_str(&choice.delta.content);
                        }
                    }
                }
            }
        }
        println!();
        Ok(full_response)
    } else {
        let request = ChatRequest {
            model: model.to_string(),
            messages: messages.to_vec(),
            stream: false,
        };

        let response = client
            .post(GROQ_API_URL)
            .header(AUTHORIZATION, format!("Bearer {}", api_key))
            .header(CONTENT_TYPE, "application/json")
            .json(&request)
            .send()
            .await?;

        let chat_response: ChatResponse = response.json().await?;
        Ok(chat_response
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .unwrap_or_default())
    }
}

fn print_welcome() {
    println!("╔════════════════════════════════════════════════════════════╗");
    println!("║             Rusty CLI • Powered by AlphsX, Inc.            ║");
    println!("╚════════════════════════════════════════════════════════════╝");
    println!();
}

fn list_models() {
    println!("Available models:");
    for (i, model) in MODELS.iter().enumerate() {
        println!("  [{}] {}", i + 1, model);
    }
    println!();
}

// Main Function

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv::dotenv().ok();

    print_welcome();

    // API Key
    let api_key = loop {
        match load_api_key() {
            Ok(k) => break k,
            Err(_) => {
                println!("API Key not found.");
                print!("Enter your GroqCloud API key: ");
                io::stdout().flush().unwrap();
                let mut input = String::new();
                io::stdin()
                    .read_line(&mut input)
                    .expect("Failed to read input");
                let key = input.trim().to_string();
                if !key.is_empty() {
                    save_api_key(&key).map_err(|e| format!("Failed to save API key: {}", e))?;
                    break key;
                }
            }
        };
    };

    list_models();

    println!("Select a model (1-3) or press Enter for default [1]: ");
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let choice = input.trim();

    // Fixes "value assigned is never read" warning by initializing directly from match
    let mut selected_model = match choice {
        "1" | "" => MODELS[0].to_string(),
        "2" => MODELS[1].to_string(),
        "3" => MODELS[2].to_string(),
        _ => {
            println!("Invalid choice. Using default model.");
            MODELS[0].to_string()
        }
    };

    println!("\nUsing model: {}\n", selected_model);
    println!("Type your message and press Enter.");
    println!("Commands: /quit, /stream, /clear, /model\n");

    let mut messages: Vec<Message> = Vec::new();
    let mut stream_mode = false;

    // Main Chat Loop
    loop {
        print!("> ");
        io::stdout().flush()?;

        // let stdin = io::stdin();
        let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
        let mut line = String::new();

        // Async reader
        reader.read_line(&mut line).await?;

        let trimmed = line.trim();

        if trimmed.is_empty() {
            continue;
        }

        // Handle Commands
        if trimmed == "/quit" || trimmed == "/exit" {
            println!("Goodbye! 🦀");
            break;
        }

        if trimmed == "/stream" {
            stream_mode = !stream_mode;
            println!("Streaming mode: {}", if stream_mode { "ON" } else { "OFF" });
            continue;
        }

        if trimmed == "/clear" {
            messages.clear();
            println!("Conversation cleared.");
            continue;
        }

        if trimmed == "/help" {
            println!("Commands:");
            println!("  /quit, /exit - Exit the chat");
            println!("  /stream      - Toggle streaming mode");
            println!("  /clear       - Clear conversation history");
            println!("  /model       - Change model");
            continue;
        }

        if trimmed == "/model" {
            list_models();
            print!("Select a model (1-3): ");
            io::stdout().flush().unwrap();
            let mut model_input = String::new();
            io::stdin().read_line(&mut model_input)?;
            match model_input.trim() {
                "1" => selected_model = MODELS[0].to_string(),
                "2" => selected_model = MODELS[1].to_string(),
                "3" => selected_model = MODELS[2].to_string(),
                _ => println!("Invalid choice. Model unchanged."),
            }
            continue;
        }

        // Add User Message
        messages.push(Message {
            role: "user".to_string(),
            content: trimmed.to_string(),
        });

        // Send Request
        match chat_completion(&api_key, &selected_model, &messages, stream_mode).await {
            Ok(response) => {
                if !stream_mode {
                    println!("\nAssistant: {}", response);
                } else {
                    println!();
                }
                messages.push(Message {
                    role: "assistant".to_string(),
                    content: response,
                });
            }
            Err(e) => {
                eprintln!("\nError: {}", e);
                messages.pop();
            }
        }
    }

    Ok(())
}
