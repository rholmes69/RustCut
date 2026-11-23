//! The Google account panel shared by the Google Drive, YouTube and Calendar windows: the
//! one-time client setup, "Sign in with Google" for the permission a window needs, the
//! connected account and Disconnect. See `crate::google`.

use crate::app::EditorApp;
use crate::google::{self, Account};
use crate::task::{Task, TaskState};
use crate::ui::widgets;
use egui::{Context, RichText, TextEdit, Ui};
use std::sync::{Arc, Mutex};

const SETUP_HELP: &str = "One-time setup (about 5 minutes, free):\n\
1. Go to console.cloud.google.com and create a project (any name, e.g. RustCut).\n\
2. APIs & Services > Library: enable the APIs you'll use: Google Drive API, YouTube Data API v3, Google Calendar API.\n\
3. OAuth consent screen (Google Auth Platform): External, and add your own Google address as a test user.\n\
4. Clients > Create client > Application type: Desktop app > Create, then Download JSON.\n\
5. Click Load client file… below and pick that JSON (or paste the id and secret).\n\
While the consent screen is in Testing, Google ends the sign-in after 7 days; click Publish app on the Audience page to stay signed in. Google then shows an \"unverified app\" notice on sign-in: click Advanced > Go to (your app).";

#[derive(Default)]
pub struct GoogleConnect {
    account: Option<Account>,
    client_id: String,
    client_secret: String,
    show_secret: bool,
    sign_in: Option<(&'static str, Task<Account>)>,
    /// The consent page, shown as a link in case the browser didn't open.
    url: Arc<Mutex<String>>,
    pub error: Option<String>,
}

impl GoogleConnect {
    pub fn account(&mut self) -> &Account {
        if self.account.is_none() {
            let a = google::load();
            self.client_id = a.client_id.clone();
            self.client_secret = a.client_secret.clone();
            self.account = Some(a);
        }
        self.account.as_ref().unwrap()
    }

    /// Signed in and allowed `scope`.
    pub fn can(&mut self, scope: &str) -> bool {
        self.account().can(scope)
    }

    pub fn signing_in(&self) -> bool {
        self.sign_in.is_some()
    }

    /// Re-reads the saved account (after an upload found the sign-in expired, say).
    pub fn reload(&mut self) {
        self.account = None;
    }
}

pub fn poll(app: &mut EditorApp) {
    let Some(state) = app.google.sign_in.as_ref().and_then(|(_, t)| t.take_finished()) else { return };
    let (scope, _) = app.google.sign_in.take().unwrap();
    match state {
        TaskState::Done(a) => {
            let who = a.label(scope).map(str::to_string).unwrap_or_else(|| "your Google account".to_string());
            app.google.account = Some(a);
            app.google.error = None;
            app.notify(format!("Connected {} for {}", who, google::scope_label(scope)));
        }
        TaskState::Failed(e) => {
            app.google.reload();
            app.google.error = Some(e);
        }
        _ => {}
    }
}

/// Who `scope` is connected as, with Switch account and Disconnect. Returns true when it was
/// disconnected.
pub fn account_line(ui: &mut Ui, ctx: &Context, g: &mut GoogleConnect, scope: &'static str) -> bool {
    let who = g.account().label(scope).map(str::to_string);
    let mut gone = false;
    ui.horizontal_wrapped(|ui| {
        let what = if scope == google::SCOPE_YOUTUBE { "Uploading to" } else { "Connected to" };
        ui.label(widgets::dim(match &who {
            Some(w) => format!("{what} {w}"),
            None => format!("{what} your Google account"),
        }));
        if g.sign_in.is_some() {
            return;
        }
        let switch_tip = if scope == google::SCOPE_YOUTUBE {
            "Sign in again and pick another Google account or brand channel for YouTube"
        } else {
            "Sign in again with another Google account for this"
        };
        if ui.small_button("Switch account").on_hover_text(switch_tip).clicked() {
            start(ctx, g, scope);
        }
        if ui.small_button("Disconnect").on_hover_text(format!("Forget the {} sign-in on this computer (the others stay)", google::scope_label(scope))).clicked() {
            match google::sign_out(scope) {
                Ok(a) => g.account = Some(a),
                Err(e) => g.error = Some(format!("{e:#}")),
            }
            gone = true;
        }
    });
    if g.sign_in.is_some() {
        waiting(ui, ctx, g);
    } else if let Some(e) = &g.error {
        ui.colored_label(crate::ui::theme::DANGER, e);
    }
    gone
}

/// While the browser sign-in is open: the link (to open or copy into another Chrome
/// profile, e.g. the one signed in to the YouTube channel's account) and Cancel.
fn waiting(ui: &mut Ui, ctx: &Context, g: &GoogleConnect) {
    ui.horizontal(|ui| {
        ui.spinner();
        ui.label("Waiting for you to allow access in the browser…");
    });
    let url = g.url.lock().unwrap().clone();
    ui.horizontal(|ui| {
        if !url.is_empty() {
            ui.hyperlink_to("Open it again", url.clone());
            if ui.button("📋 Copy sign-in link").on_hover_text("Paste it into the Chrome profile (or window) that's signed in to the account you want").clicked() {
                ctx.copy_text(url);
            }
        }
        if ui.button("Cancel").clicked() {
            if let Some((_, job)) = &g.sign_in {
                job.cancel();
            }
        }
    });
    ui.label(widgets::dim("The link works for 5 minutes, in any browser on this computer."));
    ctx.request_repaint_after(std::time::Duration::from_millis(250));
}

/// The sign-in panel for a window that needs `scope`. Draw it when `g.can(scope)` is false.
pub fn connect_ui(ui: &mut Ui, ctx: &Context, g: &mut GoogleConnect, scope: &'static str) {
    if let Some(e) = &g.error {
        ui.colored_label(crate::ui::theme::DANGER, e);
        if e.contains("disabled") || e.contains("has not been used") || e.contains("accessNotConfigured") {
            ui.label(widgets::dim("Turn the API on in Google Cloud Console (APIs & Services > Library), wait a minute, then try again."));
        }
        ui.add_space(6.0);
    }
    if g.sign_in.is_some() {
        waiting(ui, ctx, g);
        return;
    }

    let account = g.account().clone();
    if account.connected() {
        // Signed in already, just not for this: one more consent screen.
        ui.label(format!("Allow RustCut to use {}.", google::scope_label(scope)));
        if scope == google::SCOPE_YOUTUBE {
            ui.label(widgets::dim(
                "Google asks which account to use: pick the one that owns the channel, then the channel itself (e.g. a brand channel). It can be a different account from Drive's. If the account isn't yours, add it as a test user on the consent screen first.",
            ));
        } else {
            ui.label(widgets::dim("Google asks which account to use; it can be a different one from your other features."));
        }
        ui.label(widgets::dim("Make sure the matching API is enabled in your Google Cloud project (APIs & Services > Library)."));
        ui.add_space(4.0);
        if ui.button(RichText::new(format!("Allow {}", google::scope_label(scope))).strong()).clicked() {
            start(ctx, g, scope);
        }
        return;
    }

    ui.label(RichText::new(format!("Connect your Google account to use {}.", google::scope_label(scope))).strong());
    ui.collapsing("How to get a client id (one time)", |ui| {
        ui.label(widgets::dim(SETUP_HELP));
        ui.hyperlink_to("Open Google Cloud Console", "https://console.cloud.google.com/auth/clients");
    });
    ui.add_space(4.0);
    egui::Grid::new(("google-client", scope)).num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
        ui.label("Client id");
        ui.add(TextEdit::singleline(&mut g.client_id).desired_width(300.0).hint_text("…apps.googleusercontent.com"));
        ui.end_row();
        ui.label("Client secret");
        ui.horizontal(|ui| {
            ui.add(TextEdit::singleline(&mut g.client_secret).password(!g.show_secret).desired_width(240.0));
            ui.checkbox(&mut g.show_secret, "Show");
        });
        ui.end_row();
    });
    ui.horizontal(|ui| {
        if ui.button("Load client file…").on_hover_text("The JSON Google Cloud Console downloads for a Desktop app client").clicked() {
            if let Some(path) = rfd::FileDialog::new().add_filter("OAuth client JSON", &["json"]).pick_file() {
                match std::fs::read_to_string(&path).map_err(anyhow::Error::from).and_then(|j| google::parse_client_file(&j)) {
                    Ok((id, secret)) => {
                        g.client_id = id;
                        g.client_secret = secret;
                        g.error = None;
                    }
                    Err(e) => g.error = Some(format!("{e:#}")),
                }
            }
        }
        let ready = !g.client_id.trim().is_empty() && !g.client_secret.trim().is_empty();
        if ui.add_enabled(ready, egui::Button::new(RichText::new("Sign in with Google").strong())).clicked() {
            start(ctx, g, scope);
        }
    });
}

fn start(ctx: &Context, g: &mut GoogleConnect, scope: &'static str) {
    g.error = None;
    let (id, secret) = (g.client_id.trim().to_string(), g.client_secret.trim().to_string());
    let url_slot = g.url.clone();
    url_slot.lock().unwrap().clear();
    let browser = ctx.clone();
    let task = Task::spawn(ctx.clone(), move |cancel, _| {
        let open = |url: &str| {
            *url_slot.lock().unwrap() = url.to_string();
            browser.open_url(egui::OpenUrl::new_tab(url));
            browser.request_repaint();
        };
        google::sign_in(&id, &secret, scope, &open, cancel).map(Some)
    });
    g.sign_in = Some((scope, task));
}
