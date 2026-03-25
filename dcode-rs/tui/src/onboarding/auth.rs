#![allow(clippy::unwrap_used)]

use dcode_core::AuthManager;
use dcode_core::auth::AuthCredentialsStoreMode;
use dcode_core::auth::CLIENT_ID;
use dcode_core::auth::login_with_api_key;
use dcode_core::auth::read_openai_api_key_from_env;
use dcode_core::config::edit::ConfigEditsBuilder;
use dcode_login::DeviceCode;
use dcode_login::GithubCopilotDeviceCode;
use dcode_login::ServerOptions;
use dcode_login::ShutdownHandle;
use dcode_login::create_anthropic_oauth_url;
use dcode_login::exchange_anthropic_oauth_code;
use dcode_login::poll_github_copilot_token;
use dcode_login::run_login_server;
use dcode_login::start_github_copilot_auth;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::prelude::Widget;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Block;
use ratatui::widgets::BorderType;
use ratatui::widgets::Borders;
use ratatui::widgets::Paragraph;
use ratatui::widgets::WidgetRef;
use ratatui::widgets::Wrap;

use dcode_core::auth::AuthMode;
use dcode_protocol::config_types::ForcedLoginMethod;
use std::sync::RwLock;

use crate::LoginStatus;
use crate::onboarding::onboarding_screen::KeyboardHandler;
use crate::onboarding::onboarding_screen::StepStateProvider;
use crate::shimmer::shimmer_spans;
use crate::tui::FrameRequester;

/// Marks buffer cells that have cyan+underlined style as an OSC 8 hyperlink.
///
/// Terminal emulators recognise the OSC 8 escape sequence and treat the entire
/// marked region as a single clickable link, regardless of row wrapping.  This
/// is necessary because ratatui's cell-based rendering emits `MoveTo` at every
/// row boundary, which breaks normal terminal URL detection for long URLs that
/// wrap across multiple rows.
pub(crate) fn mark_url_hyperlink(buf: &mut Buffer, area: Rect, url: &str) {
    // Sanitize: strip any characters that could break out of the OSC 8
    // sequence (ESC or BEL) to prevent terminal escape injection from a
    // malformed or compromised upstream URL.
    let safe_url: String = url
        .chars()
        .filter(|&c| c != '\x1B' && c != '\x07')
        .collect();
    if safe_url.is_empty() {
        return;
    }

    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            // Only mark cells that carry the URL's distinctive style.
            if cell.fg != Color::Cyan || !cell.modifier.contains(Modifier::UNDERLINED) {
                continue;
            }
            let sym = cell.symbol().to_string();
            if sym.trim().is_empty() {
                continue;
            }
            cell.set_symbol(&format!("\x1B]8;;{safe_url}\x07{sym}\x1B]8;;\x07"));
        }
    }
}
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Notify;

use super::onboarding_screen::StepState;

mod headless_chatgpt_login;

#[derive(Clone)]
pub(crate) enum SignInState {
    PickMode,
    ChatGptContinueInBrowser(ContinueInBrowserState),
    ChatGptDeviceCode(ContinueWithDeviceCodeState),
    ChatGptSuccessMessage,
    ChatGptSuccess,
    ApiKeyEntry(ApiKeyInputState),
    ApiKeyConfigured,
    GithubCopilotDeviceCode(GithubCopilotDeviceCodeState),
    GithubCopilotConfigured,
    AnthropicApiKey(ApiKeyInputState),
    AnthropicConfigured,
    AnthropicOAuthPending {
        url: String,
        verifier: String,
        code_input: ApiKeyInputState,
    },
    AnthropicOAuthConfigured,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SignInOption {
    ChatGpt,
    DeviceCode,
    ApiKey,
    GithubCopilot,
    Anthropic,
}

/// State for the GitHub Copilot OAuth device code flow.
#[derive(Clone)]
pub(crate) struct GithubCopilotDeviceCodeState {
    pub device_code: Option<GithubCopilotDeviceCode>,
    pub cancel: Option<Arc<Notify>>,
}

const API_KEY_DISABLED_MESSAGE: &str = "API key login is disabled.";

#[derive(Clone, Default)]
pub(crate) struct ApiKeyInputState {
    value: String,
    prepopulated_from_env: bool,
}

#[derive(Clone)]
/// Used to manage the lifecycle of SpawnedLogin and ensure it gets cleaned up.
pub(crate) struct ContinueInBrowserState {
    auth_url: String,
    shutdown_flag: Option<ShutdownHandle>,
}

#[derive(Clone)]
pub(crate) struct ContinueWithDeviceCodeState {
    device_code: Option<DeviceCode>,
    cancel: Option<Arc<Notify>>,
}

impl Drop for ContinueInBrowserState {
    fn drop(&mut self) {
        if let Some(handle) = &self.shutdown_flag {
            handle.shutdown();
        }
    }
}

impl KeyboardHandler for AuthModeWidget {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if self.handle_api_key_entry_key_event(&key_event) {
            return;
        }

        match key_event.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_highlight(/*delta*/ -1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_highlight(/*delta*/ 1);
            }
            KeyCode::Char('1') => {
                self.select_option_by_index(/*index*/ 0);
            }
            KeyCode::Char('2') => {
                self.select_option_by_index(/*index*/ 1);
            }
            KeyCode::Char('3') => {
                self.select_option_by_index(/*index*/ 2);
            }
            KeyCode::Char('4') => {
                self.select_option_by_index(/*index*/ 3);
            }
            KeyCode::Char('5') => {
                self.select_option_by_index(/*index*/ 4);
            }
            KeyCode::Enter => {
                let sign_in_state = { (*self.sign_in_state.read().unwrap()).clone() };
                match sign_in_state {
                    SignInState::PickMode => {
                        self.handle_sign_in_option(self.highlighted_mode);
                    }
                    SignInState::ChatGptSuccessMessage => {
                        *self.sign_in_state.write().unwrap() = SignInState::ChatGptSuccess;
                    }
                    _ => {}
                }
            }
            KeyCode::Esc => {
                tracing::info!("Esc pressed");
                let mut sign_in_state = self.sign_in_state.write().unwrap();
                match &*sign_in_state {
                    SignInState::ChatGptContinueInBrowser(_) => {
                        *sign_in_state = SignInState::PickMode;
                        drop(sign_in_state);
                        self.request_frame.schedule_frame();
                    }
                    SignInState::ChatGptDeviceCode(state) => {
                        if let Some(cancel) = &state.cancel {
                            cancel.notify_one();
                        }
                        *sign_in_state = SignInState::PickMode;
                        drop(sign_in_state);
                        self.request_frame.schedule_frame();
                    }
                    SignInState::GithubCopilotDeviceCode(state) => {
                        if let Some(cancel) = &state.cancel {
                            cancel.notify_one();
                        }
                        *sign_in_state = SignInState::PickMode;
                        drop(sign_in_state);
                        self.request_frame.schedule_frame();
                    }
                    SignInState::AnthropicApiKey(_) | SignInState::AnthropicOAuthPending { .. } => {
                        self.error = None;
                        *sign_in_state = SignInState::PickMode;
                        drop(sign_in_state);
                        self.request_frame.schedule_frame();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn handle_paste(&mut self, pasted: String) {
        let _ = self.handle_api_key_entry_paste(pasted);
    }
}

#[derive(Clone)]
pub(crate) struct AuthModeWidget {
    pub request_frame: FrameRequester,
    pub highlighted_mode: SignInOption,
    pub error: Option<String>,
    pub sign_in_state: Arc<RwLock<SignInState>>,
    pub dcode_home: PathBuf,
    pub cli_auth_credentials_store_mode: AuthCredentialsStoreMode,
    pub login_status: LoginStatus,
    pub auth_manager: Arc<AuthManager>,
    pub forced_chatgpt_workspace_id: Option<String>,
    pub forced_login_method: Option<ForcedLoginMethod>,
    pub animations_enabled: bool,
}

impl AuthModeWidget {
    fn is_api_login_allowed(&self) -> bool {
        !matches!(self.forced_login_method, Some(ForcedLoginMethod::Chatgpt))
    }

    fn is_chatgpt_login_allowed(&self) -> bool {
        !matches!(self.forced_login_method, Some(ForcedLoginMethod::Api))
    }

    fn displayed_sign_in_options(&self) -> Vec<SignInOption> {
        let mut options = vec![SignInOption::ChatGpt];
        if self.is_chatgpt_login_allowed() {
            options.push(SignInOption::DeviceCode);
        }
        if self.is_api_login_allowed() {
            options.push(SignInOption::ApiKey);
        }
        options.push(SignInOption::GithubCopilot);
        options.push(SignInOption::Anthropic);
        options
    }

    fn selectable_sign_in_options(&self) -> Vec<SignInOption> {
        let mut options = Vec::new();
        if self.is_chatgpt_login_allowed() {
            options.push(SignInOption::ChatGpt);
            options.push(SignInOption::DeviceCode);
        }
        if self.is_api_login_allowed() {
            options.push(SignInOption::ApiKey);
        }
        options.push(SignInOption::GithubCopilot);
        options.push(SignInOption::Anthropic);
        options
    }

    fn move_highlight(&mut self, delta: isize) {
        let options = self.selectable_sign_in_options();
        if options.is_empty() {
            return;
        }

        let current_index = options
            .iter()
            .position(|option| *option == self.highlighted_mode)
            .unwrap_or(0);
        let next_index =
            (current_index as isize + delta).rem_euclid(options.len() as isize) as usize;
        self.highlighted_mode = options[next_index];
    }

    fn select_option_by_index(&mut self, index: usize) {
        let options = self.displayed_sign_in_options();
        if let Some(option) = options.get(index).copied() {
            self.handle_sign_in_option(option);
        }
    }

    fn handle_sign_in_option(&mut self, option: SignInOption) {
        match option {
            SignInOption::ChatGpt => {
                if self.is_chatgpt_login_allowed() {
                    self.start_chatgpt_login();
                }
            }
            SignInOption::DeviceCode => {
                if self.is_chatgpt_login_allowed() {
                    self.start_device_code_login();
                }
            }
            SignInOption::ApiKey => {
                if self.is_api_login_allowed() {
                    self.start_api_key_entry();
                } else {
                    self.disallow_api_login();
                }
            }
            SignInOption::GithubCopilot => {
                self.start_github_copilot_login();
            }
            SignInOption::Anthropic => {
                self.start_anthropic_api_key_entry();
            }
        }
    }

    fn disallow_api_login(&mut self) {
        self.highlighted_mode = SignInOption::ChatGpt;
        self.error = Some(API_KEY_DISABLED_MESSAGE.to_string());
        *self.sign_in_state.write().unwrap() = SignInState::PickMode;
        self.request_frame.schedule_frame();
    }

    fn render_pick_mode(&self, area: Rect, buf: &mut Buffer) {
        let mut lines: Vec<Line> = vec![
            Line::from(vec![
                "  ".into(),
                "Sign in with ChatGPT to use Dcode as part of your paid plan".into(),
            ]),
            Line::from(vec![
                "  ".into(),
                "or connect an API key for usage-based billing".into(),
            ]),
            "".into(),
        ];

        let create_mode_item = |idx: usize,
                                selected_mode: SignInOption,
                                text: &str,
                                description: &str|
         -> Vec<Line<'static>> {
            let is_selected = self.highlighted_mode == selected_mode;
            let caret = if is_selected { ">" } else { " " };

            let line1 = if is_selected {
                Line::from(vec![
                    format!("{caret} {index}. ", index = idx + 1).cyan().dim(),
                    text.to_string().cyan(),
                ])
            } else {
                format!("  {index}. {text}", index = idx + 1).into()
            };

            let line2 = if is_selected {
                Line::from(format!("     {description}"))
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::DIM)
            } else {
                Line::from(format!("     {description}"))
                    .style(Style::default().add_modifier(Modifier::DIM))
            };

            vec![line1, line2]
        };

        let chatgpt_description = if !self.is_chatgpt_login_allowed() {
            "ChatGPT login is disabled"
        } else {
            "Usage included with Plus, Pro, Business, and Enterprise plans"
        };
        let device_code_description = "Sign in from another device with a one-time code";

        for (idx, option) in self.displayed_sign_in_options().into_iter().enumerate() {
            match option {
                SignInOption::ChatGpt => {
                    lines.extend(create_mode_item(
                        idx,
                        option,
                        "Sign in with ChatGPT",
                        chatgpt_description,
                    ));
                }
                SignInOption::DeviceCode => {
                    lines.extend(create_mode_item(
                        idx,
                        option,
                        "Sign in with Device Code",
                        device_code_description,
                    ));
                }
                SignInOption::ApiKey => {
                    lines.extend(create_mode_item(
                        idx,
                        option,
                        "Provide your own API key",
                        "Pay for what you use",
                    ));
                }
                SignInOption::GithubCopilot => {
                    lines.extend(create_mode_item(
                        idx,
                        option,
                        "Sign in with GitHub Copilot",
                        "Use your GitHub Copilot subscription",
                    ));
                }
                SignInOption::Anthropic => {
                    lines.extend(create_mode_item(
                        idx,
                        option,
                        "Sign in with Anthropic API key",
                        "Paste your sk-ant-... key from console.anthropic.com",
                    ));
                }
            }
            lines.push("".into());
        }

        if !self.is_api_login_allowed() {
            lines.push(
                "  API key login is disabled by this workspace. Sign in with ChatGPT to continue."
                    .dim()
                    .into(),
            );
            lines.push("".into());
        }
        lines.push(
            // AE: Following styles.md, this should probably be Cyan because it's a user input tip.
            //     But leaving this for a future cleanup.
            "  Press Enter to continue".dim().into(),
        );
        if let Some(err) = &self.error {
            lines.push("".into());
            lines.push(err.as_str().red().into());
        }

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_continue_in_browser(&self, area: Rect, buf: &mut Buffer) {
        let mut spans = vec!["  ".into()];
        if self.animations_enabled {
            // Schedule a follow-up frame to keep the shimmer animation going.
            self.request_frame
                .schedule_frame_in(std::time::Duration::from_millis(100));
            spans.extend(shimmer_spans("Finish signing in via your browser"));
        } else {
            spans.push("Finish signing in via your browser".into());
        }
        let mut lines = vec![spans.into(), "".into()];

        let sign_in_state = self.sign_in_state.read().unwrap();
        let auth_url = if let SignInState::ChatGptContinueInBrowser(state) = &*sign_in_state
            && !state.auth_url.is_empty()
        {
            lines.push("  If the link doesn't open automatically, open the following link to authenticate:".into());
            lines.push("".into());
            lines.push(Line::from(vec![
                "  ".into(),
                state.auth_url.as_str().cyan().underlined(),
            ]));
            lines.push("".into());
            lines.push(Line::from(vec![
                "  On a remote or headless machine? Press Esc and choose ".into(),
                "Sign in with Device Code".cyan(),
                ".".into(),
            ]));
            lines.push("".into());
            Some(state.auth_url.clone())
        } else {
            None
        };

        lines.push("  Press Esc to cancel".dim().into());
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);

        // Wrap cyan+underlined URL cells with OSC 8 so the terminal treats
        // the entire region as a single clickable hyperlink.
        if let Some(url) = &auth_url {
            mark_url_hyperlink(buf, area, url);
        }
    }

    fn render_chatgpt_success_message(&self, area: Rect, buf: &mut Buffer) {
        let lines = vec![
            "✓ Signed in with your ChatGPT account".fg(Color::Green).into(),
            "".into(),
            "  Before you start:".into(),
            "".into(),
            "  Decide how much autonomy you want to grant Dcode".into(),
            Line::from(vec![
                "  For more details see the ".into(),
                "\u{1b}]8;;https://developers.openai.com/dcode/security\u{7}Dcode docs\u{1b}]8;;\u{7}".underlined(),
            ])
            .dim(),
            "".into(),
            "  Dcode can make mistakes".into(),
            "  Review the code it writes and commands it runs".dim().into(),
            "".into(),
            "  Powered by your ChatGPT account".into(),
            Line::from(vec![
                "  Uses your plan's rate limits and ".into(),
                "\u{1b}]8;;https://chatgpt.com/#settings\u{7}training data preferences\u{1b}]8;;\u{7}".underlined(),
            ])
            .dim(),
            "".into(),
            "  Press Enter to continue".fg(Color::Cyan).into(),
        ];

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_chatgpt_success(&self, area: Rect, buf: &mut Buffer) {
        let lines = vec![
            "✓ Signed in with your ChatGPT account"
                .fg(Color::Green)
                .into(),
        ];

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_api_key_configured(&self, area: Rect, buf: &mut Buffer) {
        let lines = vec![
            "✓ API key configured".fg(Color::Green).into(),
            "".into(),
            "  Dcode will use usage-based billing with your API key.".into(),
        ];

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_github_copilot_device_code(
        &self,
        area: Rect,
        buf: &mut Buffer,
        state: &GithubCopilotDeviceCodeState,
    ) {
        let banner = if state.device_code.is_some() {
            "Authorize dcode in your browser"
        } else {
            "Preparing GitHub Copilot login"
        };

        let mut spans = vec!["  ".into()];
        if self.animations_enabled {
            self.request_frame
                .schedule_frame_in(std::time::Duration::from_millis(100));
            spans.extend(crate::shimmer::shimmer_spans(banner));
        } else {
            spans.push(banner.into());
        }

        let mut lines = vec![spans.into(), "".into()];

        let verification_uri = if let Some(device_code) = &state.device_code {
            lines.push("  1. Open this link in your browser and sign in with GitHub".into());
            lines.push("".into());
            lines.push(Line::from(vec![
                "  ".into(),
                device_code.verification_uri.as_str().cyan().underlined(),
            ]));
            lines.push("".into());
            lines.push("  2. Enter this one-time code when prompted".into());
            lines.push("".into());
            lines.push(Line::from(vec![
                "  ".into(),
                device_code.user_code.as_str().cyan().bold(),
            ]));
            lines.push("".into());
            lines.push(
                "  Device codes are a common phishing target. Never share this code."
                    .dim()
                    .into(),
            );
            lines.push("".into());
            Some(device_code.verification_uri.clone())
        } else {
            lines.push("  Requesting a one-time code from GitHub...".dim().into());
            lines.push("".into());
            None
        };

        lines.push("  Press Esc to cancel".dim().into());
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);

        if let Some(url) = &verification_uri {
            mark_url_hyperlink(buf, area, url);
        }
    }

    fn render_github_copilot_configured(&self, area: Rect, buf: &mut Buffer) {
        let lines = vec![
            "✓ Signed in with GitHub Copilot".fg(Color::Green).into(),
            "".into(),
            "  Provider set to GitHub Copilot. Restart dcode to start using it.".into(),
        ];
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_anthropic_api_key_entry(
        &self,
        area: Rect,
        buf: &mut Buffer,
        state: &ApiKeyInputState,
    ) {
        let [intro_area, input_area, footer_area] = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(3),
            Constraint::Min(2),
        ])
        .areas(area);

        let intro_lines: Vec<Line> = vec![
            Line::from(vec![
                "> ".into(),
                "Sign in with your Anthropic API key".bold(),
            ]),
            "".into(),
            "  Get your key at console.anthropic.com → API keys.".into(),
            "  It will be stored locally in auth.json.".into(),
            "".into(),
        ];
        Paragraph::new(intro_lines)
            .wrap(Wrap { trim: false })
            .render(intro_area, buf);

        let content_line: Line = if state.value.is_empty() {
            vec!["Paste or type your Anthropic API key".dim()].into()
        } else {
            Line::from(state.value.clone())
        };
        Paragraph::new(content_line)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title("Anthropic API key")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .render(input_area, buf);

        let mut footer_lines: Vec<Line> = vec![
            "  Press Enter to save".dim().into(),
            "  Press Esc to go back".dim().into(),
        ];
        if let Some(error) = &self.error {
            footer_lines.push("".into());
            footer_lines.push(error.as_str().red().into());
        }
        Paragraph::new(footer_lines)
            .wrap(Wrap { trim: false })
            .render(footer_area, buf);
    }

    fn render_anthropic_configured(&self, area: Rect, buf: &mut Buffer) {
        let lines = vec![
            "✓ Anthropic API key configured".fg(Color::Green).into(),
            "".into(),
            "  Provider set to Anthropic. Restart dcode to start using it.".into(),
        ];
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_api_key_entry(&self, area: Rect, buf: &mut Buffer, state: &ApiKeyInputState) {
        let [intro_area, input_area, footer_area] = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(3),
            Constraint::Min(2),
        ])
        .areas(area);

        let mut intro_lines: Vec<Line> = vec![
            Line::from(vec![
                "> ".into(),
                "Use your own OpenAI API key for usage-based billing".bold(),
            ]),
            "".into(),
            "  Paste or type your API key below. It will be stored locally in auth.json.".into(),
            "".into(),
        ];
        if state.prepopulated_from_env {
            intro_lines.push("  Detected OPENAI_API_KEY environment variable.".into());
            intro_lines.push(
                "  Paste a different key if you prefer to use another account."
                    .dim()
                    .into(),
            );
            intro_lines.push("".into());
        }
        Paragraph::new(intro_lines)
            .wrap(Wrap { trim: false })
            .render(intro_area, buf);

        let content_line: Line = if state.value.is_empty() {
            vec!["Paste or type your API key".dim()].into()
        } else {
            Line::from(state.value.clone())
        };
        Paragraph::new(content_line)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title("API key")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .render(input_area, buf);

        let mut footer_lines: Vec<Line> = vec![
            "  Press Enter to save".dim().into(),
            "  Press Esc to go back".dim().into(),
        ];
        if let Some(error) = &self.error {
            footer_lines.push("".into());
            footer_lines.push(error.as_str().red().into());
        }
        Paragraph::new(footer_lines)
            .wrap(Wrap { trim: false })
            .render(footer_area, buf);
    }

    fn handle_api_key_entry_key_event(&mut self, key_event: &KeyEvent) -> bool {
        // Check if we're in Anthropic API key entry or OAuth code entry first.
        {
            let guard = self.sign_in_state.read().unwrap();
            if matches!(&*guard, SignInState::AnthropicApiKey(_)) {
                drop(guard);
                return self.handle_anthropic_key_entry_key_event(key_event);
            }
            if matches!(&*guard, SignInState::AnthropicOAuthPending { .. }) {
                drop(guard);
                return self.handle_anthropic_oauth_code_key_event(key_event);
            }
        }

        let mut should_save: Option<String> = None;
        let mut should_request_frame = false;

        {
            let mut guard = self.sign_in_state.write().unwrap();
            if let SignInState::ApiKeyEntry(state) = &mut *guard {
                match key_event.code {
                    KeyCode::Esc => {
                        *guard = SignInState::PickMode;
                        self.error = None;
                        should_request_frame = true;
                    }
                    KeyCode::Enter => {
                        let trimmed = state.value.trim().to_string();
                        if trimmed.is_empty() {
                            self.error = Some("API key cannot be empty".to_string());
                            should_request_frame = true;
                        } else {
                            should_save = Some(trimmed);
                        }
                    }
                    KeyCode::Backspace => {
                        if state.prepopulated_from_env {
                            state.value.clear();
                            state.prepopulated_from_env = false;
                        } else {
                            state.value.pop();
                        }
                        self.error = None;
                        should_request_frame = true;
                    }
                    KeyCode::Char(c)
                        if key_event.kind == KeyEventKind::Press
                            && !key_event.modifiers.contains(KeyModifiers::SUPER)
                            && !key_event.modifiers.contains(KeyModifiers::CONTROL)
                            && !key_event.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        if state.prepopulated_from_env {
                            state.value.clear();
                            state.prepopulated_from_env = false;
                        }
                        state.value.push(c);
                        self.error = None;
                        should_request_frame = true;
                    }
                    _ => {}
                }
                // handled; let guard drop before potential save
            } else {
                return false;
            }
        }

        if let Some(api_key) = should_save {
            self.save_api_key(api_key);
        } else if should_request_frame {
            self.request_frame.schedule_frame();
        }
        true
    }

    fn handle_api_key_entry_paste(&mut self, pasted: String) -> bool {
        let trimmed = pasted.trim();
        if trimmed.is_empty() {
            return false;
        }

        let mut guard = self.sign_in_state.write().unwrap();
        let handled = if let SignInState::ApiKeyEntry(state) = &mut *guard {
            if state.prepopulated_from_env {
                state.value = trimmed.to_string();
                state.prepopulated_from_env = false;
            } else {
                state.value.push_str(trimmed);
            }
            self.error = None;
            true
        } else if let SignInState::AnthropicApiKey(state) = &mut *guard {
            state.value.push_str(trimmed);
            self.error = None;
            true
        } else if let SignInState::AnthropicOAuthPending { code_input, .. } = &mut *guard {
            code_input.value.push_str(trimmed);
            self.error = None;
            true
        } else {
            false
        };

        if handled {
            drop(guard);
            self.request_frame.schedule_frame();
        }
        handled
    }

    fn start_api_key_entry(&mut self) {
        if !self.is_api_login_allowed() {
            self.disallow_api_login();
            return;
        }
        self.error = None;
        let prefill_from_env = read_openai_api_key_from_env();
        let mut guard = self.sign_in_state.write().unwrap();
        match &mut *guard {
            SignInState::ApiKeyEntry(state) => {
                if state.value.is_empty() {
                    if let Some(prefill) = prefill_from_env {
                        state.value = prefill;
                        state.prepopulated_from_env = true;
                    } else {
                        state.prepopulated_from_env = false;
                    }
                }
            }
            _ => {
                *guard = SignInState::ApiKeyEntry(ApiKeyInputState {
                    value: prefill_from_env.clone().unwrap_or_default(),
                    prepopulated_from_env: prefill_from_env.is_some(),
                });
            }
        }
        drop(guard);
        self.request_frame.schedule_frame();
    }

    fn save_api_key(&mut self, api_key: String) {
        if !self.is_api_login_allowed() {
            self.disallow_api_login();
            return;
        }
        match login_with_api_key(
            &self.dcode_home,
            &api_key,
            self.cli_auth_credentials_store_mode,
        ) {
            Ok(()) => {
                self.error = None;
                self.login_status = LoginStatus::AuthMode(AuthMode::ApiKey);
                self.auth_manager.reload();
                *self.sign_in_state.write().unwrap() = SignInState::ApiKeyConfigured;
            }
            Err(err) => {
                self.error = Some(format!("Failed to save API key: {err}"));
                let mut guard = self.sign_in_state.write().unwrap();
                if let SignInState::ApiKeyEntry(existing) = &mut *guard {
                    if existing.value.is_empty() {
                        existing.value.push_str(&api_key);
                    }
                    existing.prepopulated_from_env = false;
                } else {
                    *guard = SignInState::ApiKeyEntry(ApiKeyInputState {
                        value: api_key,
                        prepopulated_from_env: false,
                    });
                }
            }
        }

        self.request_frame.schedule_frame();
    }

    fn handle_anthropic_key_entry_key_event(&mut self, key_event: &KeyEvent) -> bool {
        let mut should_save: Option<String> = None;
        let mut should_request_frame = false;

        {
            let mut guard = self.sign_in_state.write().unwrap();
            if let SignInState::AnthropicApiKey(state) = &mut *guard {
                match key_event.code {
                    KeyCode::Esc => {
                        *guard = SignInState::PickMode;
                        self.error = None;
                        should_request_frame = true;
                    }
                    KeyCode::Enter => {
                        let trimmed = state.value.trim().to_string();
                        if trimmed.is_empty() {
                            self.error = Some("API key cannot be empty".to_string());
                            should_request_frame = true;
                        } else {
                            should_save = Some(trimmed);
                        }
                    }
                    KeyCode::Backspace => {
                        state.value.pop();
                        self.error = None;
                        should_request_frame = true;
                    }
                    KeyCode::Char(c)
                        if key_event.kind == KeyEventKind::Press
                            && !key_event.modifiers.contains(KeyModifiers::SUPER)
                            && !key_event.modifiers.contains(KeyModifiers::CONTROL)
                            && !key_event.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        state.value.push(c);
                        self.error = None;
                        should_request_frame = true;
                    }
                    _ => {}
                }
            } else {
                return false;
            }
        }

        if let Some(api_key) = should_save {
            self.save_anthropic_api_key(api_key);
        } else if should_request_frame {
            self.request_frame.schedule_frame();
        }
        true
    }

    fn start_anthropic_api_key_entry(&mut self) {
        self.error = None;
        *self.sign_in_state.write().unwrap() = SignInState::AnthropicApiKey(ApiKeyInputState {
            value: String::new(),
            prepopulated_from_env: false,
        });
        self.request_frame.schedule_frame();
    }

    fn save_anthropic_api_key(&mut self, api_key: String) {
        match login_with_api_key(
            &self.dcode_home,
            &api_key,
            self.cli_auth_credentials_store_mode,
        ) {
            Ok(()) => {
                self.error = None;
                // Auto-set model_provider to "anthropic" so the user doesn't
                // need to manually edit config.toml.
                let _ = ConfigEditsBuilder::new(&self.dcode_home)
                    .set_model_provider("anthropic")
                    .apply_blocking();
                self.auth_manager.reload();
                *self.sign_in_state.write().unwrap() = SignInState::AnthropicConfigured;
            }
            Err(err) => {
                self.error = Some(format!("Failed to save Anthropic API key: {err}"));
                *self.sign_in_state.write().unwrap() =
                    SignInState::AnthropicApiKey(ApiKeyInputState {
                        value: api_key,
                        prepopulated_from_env: false,
                    });
            }
        }
        self.request_frame.schedule_frame();
    }

    fn start_anthropic_oauth(&mut self) {
        self.error = None;
        let oauth = create_anthropic_oauth_url();
        *self.sign_in_state.write().unwrap() = SignInState::AnthropicOAuthPending {
            url: oauth.url,
            verifier: oauth.verifier,
            code_input: ApiKeyInputState::default(),
        };
        self.request_frame.schedule_frame();
    }

    fn handle_anthropic_oauth_code_key_event(&mut self, key_event: &KeyEvent) -> bool {
        let mut should_exchange: Option<(String, String)> = None; // (code, verifier)
        let mut should_request_frame = false;

        {
            let mut guard = self.sign_in_state.write().unwrap();
            if let SignInState::AnthropicOAuthPending { verifier, code_input, .. } = &mut *guard {
                match key_event.code {
                    KeyCode::Esc => {
                        *guard = SignInState::PickMode;
                        self.error = None;
                        should_request_frame = true;
                    }
                    KeyCode::Enter => {
                        let code = code_input.value.trim().to_string();
                        if code.is_empty() {
                            self.error = Some("Please paste the authorization code".to_string());
                            should_request_frame = true;
                        } else {
                            should_exchange = Some((code, verifier.clone()));
                        }
                    }
                    KeyCode::Backspace => {
                        code_input.value.pop();
                        self.error = None;
                        should_request_frame = true;
                    }
                    KeyCode::Char(c)
                        if key_event.kind == KeyEventKind::Press
                            && !key_event.modifiers.contains(KeyModifiers::SUPER)
                            && !key_event.modifiers.contains(KeyModifiers::CONTROL)
                            && !key_event.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        code_input.value.push(c);
                        self.error = None;
                        should_request_frame = true;
                    }
                    _ => {}
                }
            } else {
                return false;
            }
        }

        if let Some((code, verifier)) = should_exchange {
            self.exchange_anthropic_oauth_code(code, verifier);
        } else if should_request_frame {
            self.request_frame.schedule_frame();
        }
        true
    }

    fn exchange_anthropic_oauth_code(&mut self, code: String, verifier: String) {
        let sign_in_state = self.sign_in_state.clone();
        let request_frame = self.request_frame.clone();
        let dcode_home = self.dcode_home.clone();
        let auth_manager = self.auth_manager.clone();
        let creds_mode = self.cli_auth_credentials_store_mode;

        tokio::spawn(async move {
            match exchange_anthropic_oauth_code(&code, &verifier).await {
                Ok(access_token) => {
                    match login_with_api_key(&dcode_home, &access_token, creds_mode) {
                        Ok(()) => {
                            // Also set model_provider to "anthropic" so the user doesn't need
                            // to manually edit config.toml.
                            let _ = ConfigEditsBuilder::new(&dcode_home)
                                .set_model_provider("anthropic")
                                .apply_blocking();
                            auth_manager.reload();
                            *sign_in_state.write().unwrap() = SignInState::AnthropicOAuthConfigured;
                        }
                        Err(err) => {
                            tracing::warn!("Failed to save Anthropic OAuth token: {err}");
                            let mut guard = sign_in_state.write().unwrap();
                            if let SignInState::AnthropicOAuthPending { code_input, .. } = &mut *guard {
                                code_input.value = code;
                            }
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!("Anthropic OAuth exchange failed: {err}");
                    let mut guard = sign_in_state.write().unwrap();
                    if let SignInState::AnthropicOAuthPending { code_input, .. } = &mut *guard {
                        code_input.value = code;
                    }
                }
            }
            request_frame.schedule_frame();
        });
    }

    fn render_anthropic_oauth_pending(
        &self,
        area: Rect,
        buf: &mut Buffer,
        url: &str,
        code_input: &ApiKeyInputState,
    ) {
        let [intro_area, input_area, footer_area] = Layout::vertical([
            Constraint::Min(6),
            Constraint::Length(3),
            Constraint::Min(2),
        ])
        .areas(area);

        let intro_lines: Vec<Line> = vec![
            Line::from(vec![
                "> ".into(),
                "Sign in with Claude (OAuth)".bold(),
            ]),
            "".into(),
            "  1. Open this link in your browser and authorize dcode:".into(),
            "".into(),
            Line::from(vec!["  ".into(), url.cyan().underlined()]),
            "".into(),
            "  2. Paste the authorization code you receive below.".into(),
            "".into(),
        ];
        Paragraph::new(intro_lines)
            .wrap(Wrap { trim: false })
            .render(intro_area, buf);

        let content_line: Line = if code_input.value.is_empty() {
            vec!["Paste authorization code here".dim()].into()
        } else {
            Line::from(code_input.value.clone())
        };
        Paragraph::new(content_line)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title("Authorization code")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .render(input_area, buf);

        let mut footer_lines: Vec<Line> = vec![
            "  Press Enter to sign in".dim().into(),
            "  Press Esc to cancel".dim().into(),
        ];
        if let Some(error) = &self.error {
            footer_lines.push("".into());
            footer_lines.push(error.as_str().red().into());
        }
        Paragraph::new(footer_lines)
            .wrap(Wrap { trim: false })
            .render(footer_area, buf);

        mark_url_hyperlink(buf, intro_area, url);
    }

    fn render_anthropic_oauth_configured(&self, area: Rect, buf: &mut Buffer) {
        let lines = vec![
            "✓ Signed in with Claude (OAuth)".fg(Color::Green).into(),
            "".into(),
            "  Provider set to Anthropic. Restart dcode to start using it.".into(),
        ];
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn start_github_copilot_login(&mut self) {
        self.error = None;
        let cancel = Arc::new(Notify::new());
        *self.sign_in_state.write().unwrap() =
            SignInState::GithubCopilotDeviceCode(GithubCopilotDeviceCodeState {
                device_code: None,
                cancel: Some(cancel.clone()),
            });
        self.request_frame.schedule_frame();

        let sign_in_state = self.sign_in_state.clone();
        let request_frame = self.request_frame.clone();
        let dcode_home = self.dcode_home.clone();
        let auth_manager = self.auth_manager.clone();
        let creds_mode = self.cli_auth_credentials_store_mode;

        tokio::spawn(async move {
            // Step 1: request device code from GitHub.
            let device_code = match start_github_copilot_auth().await {
                Ok(dc) => dc,
                Err(err) => {
                    let mut guard = sign_in_state.write().unwrap();
                    if matches!(&*guard, SignInState::GithubCopilotDeviceCode(_)) {
                        *guard = SignInState::PickMode;
                    }
                    drop(guard);
                    tracing::warn!("GitHub Copilot device code request failed: {err}");
                    request_frame.schedule_frame();
                    return;
                }
            };

            // Step 2: show the user code.
            {
                let mut guard = sign_in_state.write().unwrap();
                if matches!(&*guard, SignInState::GithubCopilotDeviceCode(_)) {
                    *guard = SignInState::GithubCopilotDeviceCode(GithubCopilotDeviceCodeState {
                        device_code: Some(device_code.clone()),
                        cancel: Some(cancel.clone()),
                    });
                    request_frame.schedule_frame();
                } else {
                    return; // cancelled
                }
            }

            // Step 3: poll for the OAuth token.
            match poll_github_copilot_token(device_code, cancel).await {
                Ok(token) => {
                    match login_with_api_key(&dcode_home, &token, creds_mode) {
                        Ok(()) => {
                            // Also set model_provider to "github-copilot" automatically.
                            let _ = ConfigEditsBuilder::new(&dcode_home)
                                .set_model_provider("github-copilot")
                                .apply_blocking();
                            auth_manager.reload();
                            *sign_in_state.write().unwrap() = SignInState::GithubCopilotConfigured;
                        }
                        Err(err) => {
                            tracing::warn!("Failed to save GitHub Copilot token: {err}");
                            let mut guard = sign_in_state.write().unwrap();
                            if matches!(&*guard, SignInState::GithubCopilotDeviceCode(_)) {
                                *guard = SignInState::PickMode;
                            }
                        }
                    }
                }
                Err(err) => {
                    tracing::info!("GitHub Copilot login ended: {err}");
                    let mut guard = sign_in_state.write().unwrap();
                    if matches!(&*guard, SignInState::GithubCopilotDeviceCode(_)) {
                        *guard = SignInState::PickMode;
                    }
                }
            }
            request_frame.schedule_frame();
        });
    }

    fn handle_existing_chatgpt_login(&mut self) -> bool {
        if matches!(self.login_status, LoginStatus::AuthMode(AuthMode::Chatgpt)) {
            *self.sign_in_state.write().unwrap() = SignInState::ChatGptSuccess;
            self.request_frame.schedule_frame();
            true
        } else {
            false
        }
    }

    /// Kicks off the ChatGPT auth flow and keeps the UI state consistent with the attempt.
    fn start_chatgpt_login(&mut self) {
        // If we're already authenticated with ChatGPT, don't start a new login –
        // just proceed to the success message flow.
        if self.handle_existing_chatgpt_login() {
            return;
        }

        self.error = None;
        let opts = ServerOptions::new(
            self.dcode_home.clone(),
            CLIENT_ID.to_string(),
            self.forced_chatgpt_workspace_id.clone(),
            self.cli_auth_credentials_store_mode,
        );

        match run_login_server(opts) {
            Ok(child) => {
                let sign_in_state = self.sign_in_state.clone();
                let request_frame = self.request_frame.clone();
                let auth_manager = self.auth_manager.clone();
                tokio::spawn(async move {
                    let auth_url = child.auth_url.clone();
                    {
                        *sign_in_state.write().unwrap() =
                            SignInState::ChatGptContinueInBrowser(ContinueInBrowserState {
                                auth_url,
                                shutdown_flag: Some(child.cancel_handle()),
                            });
                    }
                    request_frame.schedule_frame();
                    let r = child.block_until_done().await;
                    match r {
                        Ok(()) => {
                            // Force the auth manager to reload the new auth information.
                            auth_manager.reload();

                            *sign_in_state.write().unwrap() = SignInState::ChatGptSuccessMessage;
                            request_frame.schedule_frame();
                        }
                        _ => {
                            *sign_in_state.write().unwrap() = SignInState::PickMode;
                            // self.error = Some(e.to_string());
                            request_frame.schedule_frame();
                        }
                    }
                });
            }
            Err(e) => {
                *self.sign_in_state.write().unwrap() = SignInState::PickMode;
                self.error = Some(e.to_string());
                self.request_frame.schedule_frame();
            }
        }
    }

    fn start_device_code_login(&mut self) {
        if self.handle_existing_chatgpt_login() {
            return;
        }

        self.error = None;
        let opts = ServerOptions::new(
            self.dcode_home.clone(),
            CLIENT_ID.to_string(),
            self.forced_chatgpt_workspace_id.clone(),
            self.cli_auth_credentials_store_mode,
        );
        headless_chatgpt_login::start_headless_chatgpt_login(self, opts);
    }
}

impl StepStateProvider for AuthModeWidget {
    fn get_step_state(&self) -> StepState {
        let sign_in_state = self.sign_in_state.read().unwrap();
        match &*sign_in_state {
            SignInState::PickMode
            | SignInState::ApiKeyEntry(_)
            | SignInState::AnthropicApiKey(_)
            | SignInState::AnthropicOAuthPending { .. }
            | SignInState::ChatGptContinueInBrowser(_)
            | SignInState::ChatGptDeviceCode(_)
            | SignInState::GithubCopilotDeviceCode(_)
            | SignInState::ChatGptSuccessMessage => StepState::InProgress,
            SignInState::ChatGptSuccess
            | SignInState::ApiKeyConfigured
            | SignInState::GithubCopilotConfigured
            | SignInState::AnthropicConfigured
            | SignInState::AnthropicOAuthConfigured => StepState::Complete,
        }
    }
}

impl WidgetRef for AuthModeWidget {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let sign_in_state = self.sign_in_state.read().unwrap();
        match &*sign_in_state {
            SignInState::PickMode => {
                self.render_pick_mode(area, buf);
            }
            SignInState::ChatGptContinueInBrowser(_) => {
                self.render_continue_in_browser(area, buf);
            }
            SignInState::ChatGptDeviceCode(state) => {
                headless_chatgpt_login::render_device_code_login(self, area, buf, state);
            }
            SignInState::ChatGptSuccessMessage => {
                self.render_chatgpt_success_message(area, buf);
            }
            SignInState::ChatGptSuccess => {
                self.render_chatgpt_success(area, buf);
            }
            SignInState::ApiKeyEntry(state) => {
                self.render_api_key_entry(area, buf, state);
            }
            SignInState::ApiKeyConfigured => {
                self.render_api_key_configured(area, buf);
            }
            SignInState::GithubCopilotDeviceCode(state) => {
                let state = state.clone();
                drop(sign_in_state);
                self.render_github_copilot_device_code(area, buf, &state);
                return;
            }
            SignInState::GithubCopilotConfigured => {
                self.render_github_copilot_configured(area, buf);
            }
            SignInState::AnthropicApiKey(state) => {
                let state = state.clone();
                drop(sign_in_state);
                self.render_anthropic_api_key_entry(area, buf, &state);
                return;
            }
            SignInState::AnthropicConfigured => {
                self.render_anthropic_configured(area, buf);
            }
            SignInState::AnthropicOAuthPending { url, code_input, .. } => {
                let url = url.clone();
                let code_input = code_input.clone();
                drop(sign_in_state);
                self.render_anthropic_oauth_pending(area, buf, &url, &code_input);
                return;
            }
            SignInState::AnthropicOAuthConfigured => {
                self.render_anthropic_oauth_configured(area, buf);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    use dcode_core::auth::AuthCredentialsStoreMode;

    fn widget_forced_chatgpt() -> (AuthModeWidget, TempDir) {
        let dcode_home = TempDir::new().unwrap();
        let dcode_home_path = dcode_home.path().to_path_buf();
        let widget = AuthModeWidget {
            request_frame: FrameRequester::test_dummy(),
            highlighted_mode: SignInOption::ChatGpt,
            error: None,
            sign_in_state: Arc::new(RwLock::new(SignInState::PickMode)),
            dcode_home: dcode_home_path.clone(),
            cli_auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            login_status: LoginStatus::NotAuthenticated,
            auth_manager: AuthManager::shared(
                dcode_home_path,
                false,
                AuthCredentialsStoreMode::File,
            ),
            forced_chatgpt_workspace_id: None,
            forced_login_method: Some(ForcedLoginMethod::Chatgpt),
            animations_enabled: true,
        };
        (widget, dcode_home)
    }

    #[test]
    fn api_key_flow_disabled_when_chatgpt_forced() {
        let (mut widget, _tmp) = widget_forced_chatgpt();

        widget.start_api_key_entry();

        assert_eq!(widget.error.as_deref(), Some(API_KEY_DISABLED_MESSAGE));
        assert!(matches!(
            &*widget.sign_in_state.read().unwrap(),
            SignInState::PickMode
        ));
    }

    #[test]
    fn saving_api_key_is_blocked_when_chatgpt_forced() {
        let (mut widget, _tmp) = widget_forced_chatgpt();

        widget.save_api_key("sk-test".to_string());

        assert_eq!(widget.error.as_deref(), Some(API_KEY_DISABLED_MESSAGE));
        assert!(matches!(
            &*widget.sign_in_state.read().unwrap(),
            SignInState::PickMode
        ));
        assert_eq!(widget.login_status, LoginStatus::NotAuthenticated);
    }

    /// Collects all buffer cell symbols that contain the OSC 8 open sequence
    /// for the given URL.  Returns the concatenated "inner" characters.
    fn collect_osc8_chars(buf: &Buffer, area: Rect, url: &str) -> String {
        let open = format!("\x1B]8;;{url}\x07");
        let close = "\x1B]8;;\x07";
        let mut chars = String::new();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                let sym = buf[(x, y)].symbol();
                if let Some(rest) = sym.strip_prefix(open.as_str())
                    && let Some(ch) = rest.strip_suffix(close)
                {
                    chars.push_str(ch);
                }
            }
        }
        chars
    }

    #[test]
    fn continue_in_browser_renders_osc8_hyperlink() {
        let (widget, _tmp) = widget_forced_chatgpt();
        let url = "https://auth.example.com/login?state=abc123";
        *widget.sign_in_state.write().unwrap() =
            SignInState::ChatGptContinueInBrowser(ContinueInBrowserState {
                auth_url: url.to_string(),
                shutdown_flag: None,
            });

        // Render into a narrow buffer so the URL wraps across multiple rows.
        let area = Rect::new(0, 0, 30, 20);
        let mut buf = Buffer::empty(area);
        widget.render_continue_in_browser(area, &mut buf);

        // Every character of the URL should be present as an OSC 8 cell.
        let found = collect_osc8_chars(&buf, area, url);
        assert_eq!(found, url, "OSC 8 hyperlink should cover the full URL");
    }

    #[test]
    fn mark_url_hyperlink_wraps_cyan_underlined_cells() {
        let url = "https://example.com";
        let area = Rect::new(0, 0, 20, 1);
        let mut buf = Buffer::empty(area);

        // Manually write some cyan+underlined characters to simulate a rendered URL.
        for (i, ch) in "example".chars().enumerate() {
            let cell = &mut buf[(i as u16, 0)];
            cell.set_symbol(&ch.to_string());
            cell.fg = Color::Cyan;
            cell.modifier = Modifier::UNDERLINED;
        }
        // Leave a plain cell that should NOT be marked.
        buf[(7, 0)].set_symbol("X");

        mark_url_hyperlink(&mut buf, area, url);

        // Each cyan+underlined cell should now carry the OSC 8 wrapper.
        let found = collect_osc8_chars(&buf, area, url);
        assert_eq!(found, "example");

        // The plain "X" cell should be untouched.
        assert_eq!(buf[(7, 0)].symbol(), "X");
    }

    #[test]
    fn mark_url_hyperlink_sanitizes_control_chars() {
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::empty(area);

        // One cyan+underlined cell to mark.
        let cell = &mut buf[(0, 0)];
        cell.set_symbol("a");
        cell.fg = Color::Cyan;
        cell.modifier = Modifier::UNDERLINED;

        // URL contains ESC and BEL that could break the OSC 8 sequence.
        let malicious_url = "https://evil.com/\x1B]8;;\x07injected";
        mark_url_hyperlink(&mut buf, area, malicious_url);

        let sym = buf[(0, 0)].symbol().to_string();
        // The sanitized URL retains `]` (printable) but strips ESC and BEL.
        let sanitized = "https://evil.com/]8;;injected";
        assert!(
            sym.contains(sanitized),
            "symbol should contain sanitized URL, got: {sym:?}"
        );
        // The injected close-sequence must not survive: \x1B and \x07 are gone.
        assert!(
            !sym.contains("\x1B]8;;\x07injected"),
            "symbol must not contain raw control chars from URL"
        );
    }
}
