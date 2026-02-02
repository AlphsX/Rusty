use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncBufReadExt;

// ============================================================================
// Constants
// ============================================================================

const GROQ_API_URL: &str = "https://api.groq.com/openai/v1/chat/completions";
const MODELS: &[&str] = &[
    "openai/gpt-oss-120b",
    "meta-llama/llama-4-maverick-17b-128e-instruct",
    "moonshotai/kimi-k2-instruct-0905",
];

// ============================================================================
// Data Models
// ============================================================================

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Message {
    role: String,
    content: String,
}

impl Message {
    fn new(role: &str, content: &str) -> Self {
        Self {
            role: role.to_string(),
            content: content.to_string(),
        }
    }

    fn user(content: &str) -> Self {
        Self::new("user", content)
    }

    fn assistant(content: &str) -> Self {
        Self::new("assistant", content)
    }
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
    #[allow(dead_code)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Delta {
    #[serde(default)]
    content: String,
}

// ============================================================================
// Configuration Manager
// ============================================================================

struct ConfigManager;

impl ConfigManager {
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
        let path = Self::get_config_path();
        let env_content = format!("GROQ_API_KEY={}", key.trim());
        fs::write(&path, env_content).map_err(|e| format!("Failed to write API key: {}", e))
    }

    fn prompt_for_api_key() -> Result<String, String> {
        println!("API Key not found.");
        print!("Enter your GroqCloud API key: ");
        io::stdout().flush().unwrap();

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .expect("Failed to read input");

        let key = input.trim().to_string();
        if key.is_empty() {
            return Err("Empty API key".to_string());
        }

        Self::save_api_key(&key)?;
        Ok(key)
    }

    fn get_or_prompt_api_key() -> String {
        loop {
            match Self::load_api_key() {
                Ok(key) => return key,
                Err(_) => {
                    if let Ok(key) = Self::prompt_for_api_key() {
                        return key;
                    }
                }
            }
        }
    }
}

// ============================================================================
// API Client
// ============================================================================

struct GroqApiClient {
    api_key: String,
    client: reqwest::Client,
}

impl GroqApiClient {
    fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: reqwest::Client::new(),
        }
    }

    async fn chat_completion(
        &self,
        model: &str,
        messages: &[Message],
        stream: bool,
    ) -> Result<String, reqwest::Error> {
        if stream {
            self.chat_completion_stream(model, messages).await
        } else {
            self.chat_completion_non_stream(model, messages).await
        }
    }

    async fn chat_completion_stream(
        &self,
        model: &str,
        messages: &[Message],
    ) -> Result<String, reqwest::Error> {
        let request = ChatRequest {
            model: model.to_string(),
            messages: messages.to_vec(),
            stream: true,
        };

        let mut response = self
            .client
            .post(GROQ_API_URL)
            .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
            .header(CONTENT_TYPE, "application/json")
            .json(&request)
            .send()
            .await?;

        let mut full_response = String::new();

        while let Some(chunk) = response.chunk().await? {
            let chunk_str = String::from_utf8_lossy(&chunk);
            for line in chunk_str.lines() {
                if let Some(content) = self.parse_stream_line(line) {
                    print!("{}", content);
                    io::stdout().flush().unwrap();
                    full_response.push_str(&content);
                }
            }
        }

        println!();
        Ok(full_response)
    }

    async fn chat_completion_non_stream(
        &self,
        model: &str,
        messages: &[Message],
    ) -> Result<String, reqwest::Error> {
        let request = ChatRequest {
            model: model.to_string(),
            messages: messages.to_vec(),
            stream: false,
        };

        let response = self
            .client
            .post(GROQ_API_URL)
            .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
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

    fn parse_stream_line(&self, line: &str) -> Option<String> {
        let line = line.strip_prefix("data: ").unwrap_or(line);

        if line.is_empty() || line == "[DONE]" {
            return None;
        }

        if let Ok(chunk_data) = serde_json::from_str::<StreamChunk>(line) {
            if let Some(choice) = chunk_data.choices.first() {
                if !choice.delta.content.is_empty() {
                    return Some(choice.delta.content.clone());
                }
            }
        }

        None
    }
}

// ============================================================================
// Model Manager
// ============================================================================

struct ModelManager {
    selected_model: String,
}

impl ModelManager {
    fn new() -> Self {
        Self {
            selected_model: MODELS[0].to_string(),
        }
    }

    fn list_models() {
        println!("Available models:");
        for (i, model) in MODELS.iter().enumerate() {
            println!("  [{}] {}", i + 1, model);
        }
        println!();
    }

    fn select_model_interactive(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        println!("Select a model (1-3) or press Enter for default [1]: ");
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        self.selected_model = self.parse_model_choice(input.trim());
        Ok(())
    }

    fn change_model_interactive(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        Self::list_models();
        print!("Select a model (1-3): ");
        io::stdout().flush().unwrap();

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        self.selected_model = self.parse_model_choice(input.trim());
        Ok(())
    }

    fn parse_model_choice(&self, choice: &str) -> String {
        match choice {
            "1" | "" => MODELS[0].to_string(),
            "2" => MODELS[1].to_string(),
            "3" => MODELS[2].to_string(),
            _ => {
                println!("Invalid choice. Using default model.");
                MODELS[0].to_string()
            }
        }
    }

    fn get_current_model(&self) -> &str {
        &self.selected_model
    }
}

// ============================================================================
// Conversation Manager
// ============================================================================

struct ConversationManager {
    messages: Vec<Message>,
    stream_mode: bool,
}

impl ConversationManager {
    fn new() -> Self {
        Self {
            messages: Vec::new(),
            stream_mode: false,
        }
    }

    fn add_user_message(&mut self, content: &str) {
        self.messages.push(Message::user(content));
    }

    fn add_assistant_message(&mut self, content: &str) {
        self.messages.push(Message::assistant(content));
    }

    fn remove_last_message(&mut self) {
        self.messages.pop();
    }

    fn clear(&mut self) {
        self.messages.clear();
    }

    fn get_messages(&self) -> &[Message] {
        &self.messages
    }

    fn toggle_stream_mode(&mut self) {
        self.stream_mode = !self.stream_mode;
    }

    fn is_stream_mode(&self) -> bool {
        self.stream_mode
    }
}

// ============================================================================
// User Interface
// ============================================================================

struct UserInterface;

impl UserInterface {
    fn print_welcome() {
        println!("╔════════════════════════════════════════════════════════════╗");
        println!("║             Rusty CLI • Powered by AlphsX, Inc.            ║");
        println!("╚════════════════════════════════════════════════════════════╝");
        println!();
    }

    fn print_instructions() {
        println!("Type your message and press Enter.");
        println!("Commands: /quit, /stream, /clear, /model\n");
    }

    fn print_help() {
        println!("Commands:");
        println!("  /quit, /exit - Exit the chat");
        println!("  /stream      - Toggle streaming mode");
        println!("  /clear       - Clear conversation history");
        println!("  /model       - Change model");
    }

    fn print_prompt() {
        print!("> ");
        io::stdout().flush().unwrap();
    }

    fn print_goodbye() {
        println!("Goodbye! 🦀");
    }

    fn print_assistant_response(response: &str) {
        println!("\nAssistant: {}", response);
    }

    fn print_error(error: &str) {
        eprintln!("\nError: {}", error);
    }
}

// ============================================================================
// Command Handler
// ============================================================================

enum Command {
    Quit,
    Stream,
    Clear,
    Model,
    Help,
    Message(String),
}

struct CommandHandler;

impl CommandHandler {
    fn parse(input: &str) -> Command {
        match input {
            "/quit" | "/exit" => Command::Quit,
            "/stream" => Command::Stream,
            "/clear" => Command::Clear,
            "/model" => Command::Model,
            "/help" => Command::Help,
            _ => Command::Message(input.to_string()),
        }
    }
}

// ============================================================================
// Chat Application
// ============================================================================

struct ChatApplication {
    api_client: GroqApiClient,
    model_manager: ModelManager,
    conversation_manager: ConversationManager,
}

impl ChatApplication {
    fn new(api_key: String) -> Self {
        Self {
            api_client: GroqApiClient::new(api_key),
            model_manager: ModelManager::new(),
            conversation_manager: ConversationManager::new(),
        }
    }

    async fn initialize(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        UserInterface::print_welcome();
        ModelManager::list_models();
        self.model_manager.select_model_interactive()?;

        println!(
            "\nUsing model: {}\n",
            self.model_manager.get_current_model()
        );
        UserInterface::print_instructions();

        Ok(())
    }

    async fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.initialize().await?;

        loop {
            UserInterface::print_prompt();

            let input = self.read_user_input().await?;
            if input.is_empty() {
                continue;
            }

            let command = CommandHandler::parse(&input);
            if !self.handle_command(command).await? {
                break;
            }
        }

        Ok(())
    }

    async fn read_user_input(&self) -> Result<String, Box<dyn std::error::Error>> {
        let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        Ok(line.trim().to_string())
    }

    async fn handle_command(
        &mut self,
        command: Command,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        match command {
            Command::Quit => {
                UserInterface::print_goodbye();
                Ok(false)
            }
            Command::Stream => {
                self.conversation_manager.toggle_stream_mode();
                println!(
                    "Streaming mode: {}",
                    if self.conversation_manager.is_stream_mode() {
                        "ON"
                    } else {
                        "OFF"
                    }
                );
                Ok(true)
            }
            Command::Clear => {
                self.conversation_manager.clear();
                println!("Conversation cleared.");
                Ok(true)
            }
            Command::Model => {
                self.model_manager.change_model_interactive()?;
                Ok(true)
            }
            Command::Help => {
                UserInterface::print_help();
                Ok(true)
            }
            Command::Message(content) => {
                self.process_message(&content).await?;
                Ok(true)
            }
        }
    }

    async fn process_message(&mut self, content: &str) -> Result<(), Box<dyn std::error::Error>> {
        self.conversation_manager.add_user_message(content);

        let result = self
            .api_client
            .chat_completion(
                self.model_manager.get_current_model(),
                self.conversation_manager.get_messages(),
                self.conversation_manager.is_stream_mode(),
            )
            .await;

        match result {
            Ok(response) => {
                if !self.conversation_manager.is_stream_mode() {
                    UserInterface::print_assistant_response(&response);
                } else {
                    println!();
                }
                self.conversation_manager.add_assistant_message(&response);
            }
            Err(e) => {
                UserInterface::print_error(&e.to_string());
                self.conversation_manager.remove_last_message();
            }
        }

        Ok(())
    }
}

// ============================================================================
// Main Entry Point
// ============================================================================

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv::dotenv().ok();

    let api_key = ConfigManager::get_or_prompt_api_key();
    let mut app = ChatApplication::new(api_key);
    app.run().await?;

    Ok(())
}
