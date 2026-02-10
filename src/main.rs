use rand::prelude::IndexedRandom;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use colored::*;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Style, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::{as_24_bit_terminal_escaped, LinesWithEndings};
use termimad::crossterm::style::Color as CrosstermColor;
use termimad::MadSkin;
use tokio::io::AsyncBufReadExt;

// Constants

const GROQ_API_URL: &str = "https://api.groq.com/openai/v1/chat/completions";
const MODELS: &[&str] = &[
    "openai/gpt-oss-120b",
    "meta-llama/llama-4-maverick-17b-128e-instruct",
    "moonshotai/kimi-k2-instruct-0905",
];

// Data Models

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Message {
    role: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ToolCall {
    id: String,
    #[serde(rename = "type")]
    r#type: String,
    function: FunctionCall,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct FunctionCall {
    name: String,
    arguments: String,
}

impl Message {
    fn new(role: &str, content: &str) -> Self {
        Self {
            role: role.to_string(),
            content: Some(content.to_string()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    fn user(content: &str) -> Self {
        Self::new("user", content)
    }

    fn assistant(content: &str) -> Self {
        Self::new("assistant", content)
    }

    fn system(content: &str) -> Self {
        Self::new("system", content)
    }

    fn tool(content: &str, id: &str) -> Self {
        Self {
            role: "tool".to_string(),
            content: Some(content.to_string()),
            tool_calls: None,
            tool_call_id: Some(id.to_string()),
        }
    }
}

#[derive(Debug, Serialize)]
struct ToolDefinition {
    r#type: String,
    function: ToolFunction,
}

#[derive(Debug, Serialize)]
struct ToolFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDefinition>>,
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

// Configuration Manager

struct ConfigManager;

impl ConfigManager {
    fn get_config_path() -> PathBuf {
        let mut current_dir = std::env::current_dir().expect("Could not get current directory");
        current_dir.push(".env");
        current_dir
    }

    fn load_key(key_name: &str) -> Result<String, String> {
        if let Ok(key) = std::env::var(key_name) {
            return Ok(key);
        }
        Err(format!("{} not found in .env file.", key_name))
    }

    fn save_key(key_name: &str, key_value: &str) -> Result<(), String> {
        let path = Self::get_config_path();
        let mut content = if path.exists() {
            fs::read_to_string(&path).unwrap_or_default()
        } else {
            String::new()
        };

        let new_line = format!("{}={}", key_name, key_value.trim());

        let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
        let mut found = false;
        for line in &mut lines {
            if line.starts_with(&format!("{}=", key_name)) {
                *line = new_line.clone();
                found = true;
                break;
            }
        }
        if !found {
            lines.push(new_line);
        }

        content = lines.join("\n");
        if !content.ends_with('\n') && !content.is_empty() {
            content.push('\n');
        }

        fs::write(&path, content).map_err(|e| format!("Failed to write API key: {}", e))
    }

    fn prompt_for_key(key_name: &str, display_name: &str) -> Result<String, String> {
        println!("{} not found.", display_name);
        print!("Enter your {}: ", display_name);
        io::stdout().flush().unwrap();

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .expect("Failed to read input");

        let key = input.trim().to_string();
        if key.is_empty() {
            return Err("Empty API key".to_string());
        }

        Self::save_key(key_name, &key)?;
        Ok(key)
    }

    fn get_or_prompt_api_keys() -> (String, String) {
        let groq_key = loop {
            match Self::load_key("GROQ_API_KEY") {
                Ok(key) => break key,
                Err(_) => {
                    if let Ok(key) = Self::prompt_for_key("GROQ_API_KEY", "GroqCloud API key") {
                        break key;
                    }
                }
            }
        };

        let brave_key = loop {
            match Self::load_key("BRAVE_API_KEY") {
                Ok(key) => break key,
                Err(_) => {
                    if let Ok(key) = Self::prompt_for_key("BRAVE_API_KEY", "Brave Search API key") {
                        break key;
                    }
                }
            }
        };

        (groq_key, brave_key)
    }
}

// Brave Search Client

struct BraveSearchClient {
    api_key: String,
    client: reqwest::Client,
}

impl BraveSearchClient {
    fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: reqwest::Client::new(),
        }
    }

    async fn search(&self, query: &str) -> Result<String, reqwest::Error> {
        let url = "https://api.search.brave.com/res/v1/web/search";
        let response = self
            .client
            .get(url)
            .header("X-Subscription-Token", &self.api_key)
            .header("Accept", "application/json")
            .query(&[("q", query), ("count", "5")])
            .send()
            .await?;

        let json: serde_json::Value = response.json().await?;
        Ok(self.format_results(json))
    }

    fn format_results(&self, json: serde_json::Value) -> String {
        let mut output = String::from("### Brave Search Results\n\n");

        if let Some(web) = json
            .get("web")
            .and_then(|w| w.get("results"))
            .and_then(|r| r.as_array())
        {
            if web.is_empty() {
                output.push_str("No results found.\n");
            } else {
                for (i, result) in web.iter().enumerate().take(5) {
                    let title = result
                        .get("title")
                        .and_then(|t| t.as_str())
                        .unwrap_or("No Title");
                    let description = result
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("");
                    let url = result.get("url").and_then(|u| u.as_str()).unwrap_or("");

                    output.push_str(&format!("{}. **{}**\n", i + 1, title));
                    output.push_str(&format!("   - Snippet: {}\n", description));
                    output.push_str(&format!("   - URL: {}\n\n", url));
                }
            }
        } else {
            output.push_str("Failed to parse search results.\n");
        }

        output
    }
}

// API Client

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
        tools: Option<Vec<ToolDefinition>>,
    ) -> Result<Message, reqwest::Error> {
        self.chat_completion_non_stream(model, messages, tools)
            .await
    }

    async fn chat_completion_non_stream(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<Vec<ToolDefinition>>,
    ) -> Result<Message, reqwest::Error> {
        let mut final_messages = Vec::new();
        final_messages.push(Message::system("You are a helpful AI assistant with access to real-time information via the `brave_search` tool. You can use it to find up-to-date information. Do not attempt to use any tools that are not listed here. Specifically, do NOT use a tool named `open` or `read_file`; they do not exist."));
        final_messages.extend_from_slice(messages);

        let request = ChatRequest {
            model: model.to_string(),
            messages: final_messages,
            stream: false,
            tools,
        };

        let mut retries = 0;
        loop {
            let response = self
                .client
                .post(GROQ_API_URL)
                .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
                .header(CONTENT_TYPE, "application/json")
                .json(&request)
                .send()
                .await?;

            if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                if retries >= 3 {
                    let body_text = response.text().await?;
                    eprintln!("Rate limit exceeded after retries. Body: {}", body_text);
                    panic!("Groq API Rate Limit Exceeded");
                }
                retries += 1;
                eprintln!(
                    "Rate limit hit, retrying in 2 seconds... (Attempt {}/3)",
                    retries
                );
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                continue;
            }

            let body_text = response.text().await?;
            let chat_response: ChatResponse = match serde_json::from_str(&body_text) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Failed to parse API response: {}", e);
                    eprintln!("Response body: {}", body_text);
                    panic!("Groq API Error: {}", e);
                }
            };
            return Ok(chat_response
                .choices
                .first()
                .map(|c| c.message.clone())
                .unwrap_or_else(|| Message::assistant("")));
        }
    }

    // Stream mode is trickier with tool calls, for now let's focus on non-stream for search
    // or handle it by disabling stream when tool calls are expected.
}

// Model Manager

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

// Conversation Manager

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

// User Interface

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
        println!("Commands: /exit, /stream, /clear, /model\n");
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
        let skin = Self::get_skin();
        println!(); // Spacing before response
        print!("● ");
        use std::io::Write;
        let _ = std::io::stdout().flush();

        // Initialize syntect
        let ps = SyntaxSet::load_defaults_newlines();
        let ts = ThemeSet::load_defaults();
        let theme = &ts.themes["base16-ocean.dark"]; // A good dark theme

        // Simple markdown splitter for code blocks
        let parts: Vec<&str> = response.split("```").collect();

        for (i, part) in parts.iter().enumerate() {
            if i % 2 == 0 {
                // Text part
                if !part.trim().is_empty() {
                    skin.print_text(part);
                }
            } else {
                // Code block part
                let mut lines = part.lines();
                let lang = lines.next().unwrap_or("").trim();
                let code = lines.collect::<Vec<&str>>().join("\n");

                if code.trim().is_empty() {
                    continue;
                }

                let syntax = ps
                    .find_syntax_by_token(lang)
                    .unwrap_or_else(|| ps.find_syntax_plain_text());

                let mut h = HighlightLines::new(syntax, theme);

                // Note: termimad has code block styling, but we are bypassing it for syntax highlighting.
                println!();

                for line in LinesWithEndings::from(&code) {
                    let ranges: Vec<(Style, &str)> = h.highlight_line(line, &ps).unwrap();
                    let escaped = as_24_bit_terminal_escaped(&ranges[..], false);
                    print!("   {}", escaped); // Indent code
                }
                println!("\x1b[0m"); // Reset colors
                println!();
            }
        }

        println!(); // Spacing after response
    }

    fn get_skin() -> MadSkin {
        let mut skin = MadSkin::default();

        let orange = CrosstermColor::AnsiValue(208);
        let dark_grey = CrosstermColor::AnsiValue(236);
        let light_yellow = CrosstermColor::AnsiValue(229);
        let white = CrosstermColor::White;
        let grey = CrosstermColor::Grey;

        skin.set_headers_fg(orange);
        skin.bold.set_fg(white);
        skin.italic.set_fg(grey);

        // Code block styling
        skin.code_block.set_bg(dark_grey);
        skin.code_block.set_fg(white);

        // Inline code styling
        skin.inline_code.set_fg(light_yellow);

        skin
    }

    fn print_step(step: &str, color: Color) {
        println!("{} {}...", "\n*".color(color), step.color(color));
    }

    fn print_error(error: &str) {
        eprintln!("\nError: {}", error);
    }
}

// Command Handler

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

// Chat Application

struct ChatApplication {
    api_client: GroqApiClient,
    brave_client: BraveSearchClient,
    model_manager: ModelManager,
    conversation_manager: ConversationManager,
    reader: tokio::io::BufReader<tokio::io::Stdin>,
}

impl ChatApplication {
    fn new(groq_key: String, brave_key: String) -> Self {
        Self {
            api_client: GroqApiClient::new(groq_key),
            brave_client: BraveSearchClient::new(brave_key),
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

        let blue = Color::TrueColor {
            r: 122,
            g: 162,
            b: 247,
        };
        let green = Color::TrueColor {
            r: 166,
            g: 227,
            b: 161,
        };

        UserInterface::print_thinking();

        loop {
            let tools = vec![self.get_brave_search_tool(), self.get_open_tool()];

            let result = self
                .api_client
                .chat_completion(
                    self.model_manager.get_current_model(),
                    self.conversation_manager.get_messages(),
                    Some(tools),
                )
                .await;

            match result {
                Ok(response_msg) => {
                    self.conversation_manager
                        .messages
                        .push(response_msg.clone());

                    if let Some(tool_calls) = &response_msg.tool_calls {
                        for tool_call in tool_calls {
                            if tool_call.function.name == "brave_search" {
                                let args: serde_json::Value =
                                    serde_json::from_str(&tool_call.function.arguments)?;
                                let query = args["query"].as_str().unwrap_or("");

                                UserInterface::print_step(
                                    &format!("Searching Brave for '{}'", query),
                                    blue,
                                );

                                match self.brave_client.search(query).await {
                                    Ok(search_results) => {
                                        UserInterface::print_step(
                                            "Reasoning with search results",
                                            green,
                                        );
                                        self.conversation_manager
                                            .messages
                                            .push(Message::tool(&search_results, &tool_call.id));
                                    }
                                    Err(e) => {
                                        UserInterface::print_error(&format!(
                                            "Search failed: {}",
                                            e
                                        ));
                                        self.conversation_manager.messages.push(Message::tool(
                                            "Error: Search failed. Please answer without search.",
                                            &tool_call.id,
                                        ));
                                    }
                                }
                            } else if tool_call.function.name == "open" {
                                let args: serde_json::Value =
                                    serde_json::from_str(&tool_call.function.arguments)?;
                                let url = args
                                    .get("id")
                                    .or_else(|| args.get("url"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");

                                UserInterface::print_step(
                                    &format!("Checking content from '{}'", url),
                                    blue,
                                );

                                // Redirect to brave search as a fallback for now
                                match self.brave_client.search(url).await {
                                    Ok(search_results) => {
                                        UserInterface::print_step("Analyzing page content", green);
                                        self.conversation_manager
                                            .messages
                                            .push(Message::tool(&search_results, &tool_call.id));
                                    }
                                    Err(e) => {
                                        UserInterface::print_error(&format!(
                                            "Failed to read content: {}",
                                            e
                                        ));
                                        self.conversation_manager.messages.push(Message::tool(
                                            "Error: Failed to read page content. Please try searching instead.",
                                            &tool_call.id,
                                        ));
                                    }
                                }
                            }
                        }
                        // Continue loop to let AI process results
                        continue;
                    } else {
                        // No more tool calls, we have final response
                        if let Some(final_content) = &response_msg.content {
                            UserInterface::print_assistant_response(final_content);
                        }
                        break;
                    }
                }
                Err(e) => {
                    UserInterface::print_error(&e.to_string());
                    self.conversation_manager.remove_last_message();
                    break;
                }
            }
        }

        Ok(())
    }

    fn get_brave_search_tool(&self) -> ToolDefinition {
        ToolDefinition {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "brave_search".to_string(),
                description: "Search the web for up-to-date information, news, current events, and general knowledge. Use this for questions that require real-time data or when you need to verify facts.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "The search query to look up on the web."
                        }
                    },
                    "required": ["query"]
                }),
            },
        }
    }

    fn get_open_tool(&self) -> ToolDefinition {
        ToolDefinition {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "open".to_string(),
                description: "Open a URL to read its content.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "id": {
                            "type": "string",
                            "description": "The URL or ID of the resource to open."
                        }
                    },
                    "required": ["id"]
                }),
            },
        }
    }
}

// Main Entry Point

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    let (groq_key, brave_key) = ConfigManager::get_or_prompt_api_keys();
    let mut app = ChatApplication::new(groq_key, brave_key);
    app.run().await?;

    Ok(())
}
