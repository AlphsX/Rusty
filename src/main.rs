use rand::prelude::IndexedRandom;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use colored::*;
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
        let orange = Color::TrueColor {
            r: 255,
            g: 140,
            b: 0,
        };
        println!("{}", "\nAvailable models:".color(orange).bold());
        for (i, model) in MODELS.iter().enumerate() {
            println!("  [{}] {}", (i + 1).to_string().color(orange), model);
        }
        println!();
    }

    async fn select_model_interactive(
        &mut self,
        reader: &mut tokio::io::BufReader<tokio::io::Stdin>,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let orange = Color::TrueColor {
            r: 255,
            g: 140,
            b: 0,
        };
        let gray = Color::TrueColor {
            r: 100,
            g: 100,
            b: 100,
        };
        println!("Select a model (1-3) or press Enter for default [1]: ");
        println!("{}", "─".repeat(110).color(gray));
        println!(" ");
        println!("{}", "─".repeat(110).color(gray));
        print!("  {} {}", "?".color(gray), "for shortcuts".color(gray));
        io::stdout().flush().unwrap();

        print!("\x1b[2A\r");
        print!("{} ", "❯".color(orange).bold());
        io::stdout().flush().unwrap();

        let mut input = String::new();
        tokio::io::AsyncBufReadExt::read_line(reader, &mut input).await?;
        let input = input.trim();

        // Move to hint line and clear it
        println!();
        print!("\x1b[2K\r");
        io::stdout().flush().unwrap();

        if input == "/exit" || input == "/quit" {
            let goodbyes = [
                "Catch you on the flip side!",
                "Keep it 100!",
                "Stay classy!",
                "Later, alligator!",
                "See ya!",
                "Cheers!",
                "Bye!",
                "Until next time!",
            ];
            let mut rng = rand::rng();
            let goodbye = goodbyes.choose(&mut rng).unwrap_or(&"Goodbye!");
            println!("  ⎿  {}\n", goodbye);
            return Ok(false);
        }

        self.selected_model = self.parse_model_choice(input);
        Ok(true)
    }

    async fn change_model_interactive(
        &mut self,
        reader: &mut tokio::io::BufReader<tokio::io::Stdin>,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let orange = Color::TrueColor {
            r: 255,
            g: 140,
            b: 0,
        };
        let gray = Color::TrueColor {
            r: 100,
            g: 100,
            b: 100,
        };
        Self::list_models();
        println!("Select a model (1-3): ");
        println!("{}", "─".repeat(110).color(gray));
        println!(" ");
        println!("{}", "─".repeat(110).color(gray));
        print!("  {} {}", "?".color(gray), "for shortcuts".color(gray));
        io::stdout().flush().unwrap();

        print!("\x1b[2A\r");
        print!("{} ", "❯".color(orange).bold());
        io::stdout().flush().unwrap();

        let mut input = String::new();
        tokio::io::AsyncBufReadExt::read_line(reader, &mut input).await?;
        let input = input.trim();

        // Move to hint line and clear it
        println!();
        print!("\x1b[2K\r");
        io::stdout().flush().unwrap();

        if input == "/exit" || input == "/quit" {
            let goodbyes = [
                "Catch you on the flip side!",
                "Keep it 100!",
                "Stay classy!",
                "Later, alligator!",
                "See ya!",
                "Cheers!",
                "Bye!",
                "Until next time!",
            ];
            let mut rng = rand::rng();
            let goodbye = goodbyes.choose(&mut rng).unwrap_or(&"Goodbye!");
            println!("  ⎿  {}\n", goodbye);
            return Ok(false);
        }

        self.selected_model = self.parse_model_choice(input);
        Ok(true)
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
    fn print_welcome(model: &str) {
        let orange = Color::TrueColor {
            r: 255,
            g: 140,
            b: 0,
        };
        println!(
            "\nLaunching {} with {}...\n",
            "Rusty".color(orange).bold(),
            model.white().bold()
        );
        // Simulate a small delay or just clear for the dashboard
        Self::draw_dashboard(model);
    }

    fn draw_dashboard(model: &str) {
        let orange = Color::TrueColor {
            r: 255,
            g: 140,
            b: 0,
        }; // Signature Rusty Orange
        let gray = Color::TrueColor {
            r: 100,
            g: 100,
            b: 100,
        };
        let logo_orange = Color::TrueColor {
            r: 233,
            g: 116,
            b: 81,
        };
        let path = "~/Claude Code/Rusty";
        let version = "v1.0.0";
        let b = "│".color(orange);
        let s = "│".color(gray);
        let inner_w = 110; // Widened to fit text
        let col1_w = 40;
        let col2_w = inner_w - col1_w - 1; // 69

        // helper for padding
        let pad = |s: &str, w: usize, center: bool| -> String {
            let s_len = s.chars().count();
            if s_len >= w {
                return s.chars().take(w).collect();
            }
            if center {
                let left = (w - s_len) / 2;
                let right = w - s_len - left;
                format!("{}{}{}", " ".repeat(left), s, " ".repeat(right))
            } else {
                format!("{}{}", s, " ".repeat(w - s_len))
            }
        };

        // Header
        println!(
            "{}",
            format!(
                "╭─── Rusty {} {}╮",
                version,
                "─".repeat(inner_w - 11 - version.len())
            )
            .color(orange)
        );

        // Blank line for padding
        println!(
            "{}{}{}{}{}",
            b,
            pad("", col1_w, false),
            s,
            pad("", col2_w, false),
            b
        );

        // Line 1: Tips header
        println!(
            "{}{}{}{}{}",
            b,
            pad("Welcome back!", col1_w, true).white().bold(),
            s,
            pad(" Tips for getting started", col2_w, false)
                .color(orange)
                .bold(),
            b
        );

        // Line 2: Instructions
        println!(
            "{}{}{}{}{}",
            b,
            pad("", col1_w, true),
            s,
            pad(
                " Run /init to create a RUSTY.md file with instructions for Rusty",
                col2_w,
                false
            )
            .white(),
            b
        );

        // Line 3: Separator
        println!(
            "{}{}{}{}{}",
            b,
            pad("", col1_w, false),
            s,
            pad(&format!(" {}", "─".repeat(63)), col2_w, false).color(gray),
            b
        );

        // Line 4: Recent activity
        println!(
            "{}{}{}{}{}",
            b,
            pad("▐▛███▜▌", col1_w, true).color(logo_orange).bold(),
            s,
            pad(" Recent activity", col2_w, false).color(orange).bold(),
            b
        );

        // Line 5: Logo Row 2 + No activity
        println!(
            "{}{}{}{}{}",
            b,
            pad("▝▜█████▛▘", col1_w, true).color(logo_orange).bold(),
            s,
            pad(" No recent activity", col2_w, false).color(gray),
            b
        );

        // Line 6: Logo Row 3
        println!(
            "{}{}{}{}{}",
            b,
            pad("▘▘ ▝▝ ", col1_w, true).color(logo_orange).bold(),
            s,
            pad("", col2_w, false),
            b
        );

        // Line 7: Model info
        let model_info = format!("{} • API Usage Billing", model);
        println!(
            "{}{}{}{}{}",
            b,
            pad(&model_info, col1_w, true).color(gray),
            s,
            pad("", col2_w, false),
            b
        );

        // Line 8: Path
        println!(
            "{}{}{}{}{}",
            b,
            pad(path, col1_w, true).color(gray),
            s,
            pad("", col2_w, false),
            b
        );

        // Blank line for padding
        println!(
            "{}{}{}{}{}",
            b,
            pad("", col1_w, false),
            s,
            pad("", col2_w, false),
            b
        );

        // Footer
        println!("{}", format!("╰{}╯", "─".repeat(inner_w)).color(orange));
    }

    fn print_instructions() {
        println!("Type your message and press Enter.");
        println!("Commands: /quit, /stream, /clear, /model\n");
    }

    fn print_help() {
        println!("  /exit                   Exit the REPL");
        println!("  /model                  Change the AI model");
        println!("  /clear                  Clear conversation history and free up context");
        println!("  /stream                 Toggle streaming mode");
        println!("  /help                   Show this help message");
        println!();
    }

    fn print_prompt() {
        let orange = Color::TrueColor {
            r: 255,
            g: 140,
            b: 0,
        };
        let gray = Color::TrueColor {
            r: 100,
            g: 100,
            b: 100,
        };

        // Sandwich Layout: Pre-render the full box and move cursor back up.
        // This makes it look like you are typing INSIDE the box.
        println!("{}", "─".repeat(110).color(gray));
        println!(" "); // Placeholder for prompt
        println!("{}", "─".repeat(110).color(gray));
        print!("  {} {}", "?".color(gray), "for shortcuts".color(gray));
        io::stdout().flush().unwrap();

        // Move cursor up 2 lines (From Hint -> Bottom -> Prompt Space)
        // Since we used print! for the hint, we are ON the hint line at the end.
        // Move up 2 to get to Prompt Space.
        print!("\x1b[2A\r");
        print!("{} ", "❯".color(orange).bold());
        io::stdout().flush().unwrap();
    }

    fn print_prompt_closure() {
        // After input, we are on the Bottom Separator line.
        // Move down to Hint Line and clear it so next output starts fresh.
        println!();
        print!("\x1b[2K\r");
        io::stdout().flush().unwrap();
    }

    fn print_thinking() {
        let colors = [
            Color::TrueColor {
                r: 242,
                g: 205,
                b: 205,
            }, // Catppuccin Flamingo
            Color::TrueColor {
                r: 187,
                g: 154,
                b: 247,
            }, // Tokyo Night Purple
            Color::TrueColor {
                r: 122,
                g: 162,
                b: 247,
            }, // Tokyo Night Blue
            Color::TrueColor {
                r: 250,
                g: 179,
                b: 135,
            }, // Catppuccin Peach
            Color::TrueColor {
                r: 156,
                g: 207,
                b: 216,
            }, // Rosé Pine Foam
            Color::TrueColor {
                r: 235,
                g: 111,
                b: 145,
            }, // Rosé Pine Rose
            Color::TrueColor {
                r: 166,
                g: 227,
                b: 161,
            }, // Catppuccin Green
            Color::TrueColor {
                r: 210,
                g: 126,
                b: 153,
            }, // Kanagawa Sakura (New)
            Color::TrueColor {
                r: 126,
                g: 156,
                b: 216,
            }, // Kanagawa Crystal (New)
            Color::TrueColor {
                r: 167,
                g: 192,
                b: 128,
            }, // Everforest Spring (New)
            Color::TrueColor {
                r: 230,
                g: 152,
                b: 117,
            }, // Everforest Ochre (New)
            Color::TrueColor {
                r: 198,
                g: 120,
                b: 221,
            }, // One Dark Magenta (New)
            Color::TrueColor {
                r: 86,
                g: 182,
                b: 194,
            }, // One Dark Cyan (New)
            Color::TrueColor {
                r: 220,
                g: 165,
                b: 97,
            }, // Kanagawa Autumn (New)
        ];

        let words = [
            "Simmering",
            "Sparkling",
            "Zesting",
            "Julienning",
            "Marinating",
            "Cerebrating",
            "Cogitating",
            "Ruminating",
            "Pondering",
            "Clauding",
            "Razzmatazzing",
        ];

        let mut rng = rand::rng();
        let word = words.choose(&mut rng).unwrap_or(&"Thinking");
        let color = colors.choose(&mut rng).unwrap_or(&colors[0]);

        println!("{} {}...", "\n*".color(*color), word.color(*color));
    }

    fn print_assistant_response(response: &str) {
        println!("● {}\n", response);
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
            "/help" | "/" | "?" => Command::Help,
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
    reader: tokio::io::BufReader<tokio::io::Stdin>,
}

impl ChatApplication {
    fn new(api_key: String) -> Self {
        Self {
            api_client: GroqApiClient::new(api_key),
            model_manager: ModelManager::new(),
            conversation_manager: ConversationManager::new(),
            reader: tokio::io::BufReader::new(tokio::io::stdin()),
        }
    }

    async fn initialize(&mut self) -> Result<bool, Box<dyn std::error::Error>> {
        UserInterface::print_welcome(self.model_manager.get_current_model());
        ModelManager::list_models();
        if !self
            .model_manager
            .select_model_interactive(&mut self.reader)
            .await?
        {
            return Ok(false);
        }

        let orange = Color::TrueColor {
            r: 255,
            g: 140,
            b: 0,
        };
        println!(
            "\n{} {}\n",
            "Active Model:".color(orange).bold(),
            self.model_manager.get_current_model().white()
        );
        UserInterface::print_instructions();

        Ok(true)
    }

    async fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if !self.initialize().await? {
            return Ok(());
        }

        // Initial hint
        // Initial hint handled by print_prompt

        loop {
            UserInterface::print_prompt();

            let input = self.read_user_input().await?;
            UserInterface::print_prompt_closure();

            if input.is_empty() {
                continue;
            }

            let command = CommandHandler::parse(&input);
            if !self.handle_command(command, &input).await? {
                break;
            }
        }

        Ok(())
    }

    async fn read_user_input(&mut self) -> Result<String, Box<dyn std::error::Error>> {
        let mut line = String::new();
        self.reader.read_line(&mut line).await?;
        Ok(line.trim().to_string())
    }

    async fn handle_command(
        &mut self,
        command: Command,
        _input: &str,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        match command {
            Command::Quit => {
                let goodbyes = [
                    "Catch you on the flip side!",
                    "Keep it 100!",
                    "Stay classy!",
                    "Later, alligator!",
                    "See ya!",
                    "Cheers!",
                    "Bye!",
                    "Until next time!",
                ];
                let mut rng = rand::rng();
                let goodbye = goodbyes.choose(&mut rng).unwrap_or(&"Goodbye!");

                println!("  ⎿  {}\n", goodbye);
                Ok(false)
            }
            Command::Stream => {
                self.conversation_manager.toggle_stream_mode();
                let status = if self.conversation_manager.is_stream_mode() {
                    "ON"
                } else {
                    "OFF"
                };
                println!("  ⎿  Streaming mode: {}\n", status);
                Ok(true)
            }
            Command::Clear => {
                self.conversation_manager.clear();
                println!("  ⎿  (no content)\n");
                Ok(true)
            }
            Command::Model => {
                if !self
                    .model_manager
                    .change_model_interactive(&mut self.reader)
                    .await?
                {
                    return Ok(false);
                }
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

        if !self.conversation_manager.is_stream_mode() {
            UserInterface::print_thinking();
        }

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
    dotenvy::dotenv().ok();

    let api_key = ConfigManager::get_or_prompt_api_key();
    let mut app = ChatApplication::new(api_key);
    app.run().await?;

    Ok(())
}
