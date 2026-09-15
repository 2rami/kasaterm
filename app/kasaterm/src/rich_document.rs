use super::*;

#[derive(Clone, Copy)]
pub(crate) enum PendingAction {
    Save,
    Raw,
    Close,
    Quit,
    AppClose,
    Navigate,
}

pub(crate) struct RichDocHost {
    view: wry::WebView,
    backing: Option<Arc<Window>>,
    parent: Arc<Window>,
    pub token: String,
    auth_token: Arc<std::sync::Mutex<String>>,
    pub revision: u64,
    pub ready: bool,
    pub pending: Option<(PendingAction, Instant, String)>,
    visible: bool,
    frame: Option<(i32, i32, u32, u32)>,
}

impl Drop for RichDocHost {
    fn drop(&mut self) {
        if let Some(backing) = &self.backing {
            crate::webpane::detach_child(&self.parent, backing);
            backing.set_visible(false);
        }
    }
}

impl RichDocHost {
    pub fn new(
        parent: Arc<Window>,
        event_loop: &ActiveEventLoop,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Result<Self, String> {
        let token = uuid::Uuid::new_v4().simple().to_string();
        let owner = parent.id();
        let auth_token = Arc::new(std::sync::Mutex::new(token.clone()));
        let expected = auth_token.clone();
        let script = include_str!("../../../document-editor/dist/editor.js")
            .replace("</script", "<\\/script");
        let css = include_str!("../../../document-editor/dist/editor.css");
        let html = format!("<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: https: http:; font-src data:;\"><style>{css}</style></head><body><script>{script}</script></body></html>");
        let builder = wry::WebViewBuilder::new()
            .with_html(html)
            .with_visible(false)
            .with_initialization_script(format!(
                "window.__KASATERM_DOC_TOKEN__={};",
                serde_json::to_string(&token).unwrap()
            ))
            .with_ipc_handler(move |request| {
                let body = request.into_body();
                if body.len() > 16 * 1024 * 1024 {
                    return;
                }
                let Ok(message) = serde_json::from_str::<serde_json::Value>(&body) else {
                    return;
                };
                if !expected.lock().is_ok_and(|token| message.get("token").and_then(|v| v.as_str()) == Some(token.as_str())) {
                    return;
                }
                let _ = proxy.send_event(UserEvent::RichDocument {
                    owner,
                    message: body,
                });
            })
            .with_navigation_handler(|url| url == "about:blank")
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny);
        #[cfg(target_os = "macos")]
        let backing = {
            use winit::platform::macos::WindowAttributesExtMacOS;
            Some(Arc::new(
                event_loop
                    .create_window(
                        WindowAttributes::default()
                            .with_title("문서 본문")
                            .with_decorations(false)
                            .with_resizable(false)
                            .with_visible(false)
                            .with_has_shadow(false),
                    )
                    .map_err(|e| e.to_string())?,
            ))
        };
        #[cfg(not(target_os = "macos"))]
        let backing: Option<Arc<Window>> = {
            let _ = event_loop;
            None
        };
        let view = builder
            .build_as_child(backing.as_ref().unwrap_or(&parent).as_ref())
            .map_err(|e| e.to_string())?;
        Ok(Self {
            view,
            backing,
            parent,
            token,
            auth_token,
            revision: 0,
            ready: false,
            pending: None,
            visible: false,
            frame: None,
        })
    }

    pub fn owns(&self, id: WindowId) -> bool {
        self.backing.as_ref().is_some_and(|w| w.id() == id)
    }

    pub fn call(&self, method: &str, value: serde_json::Value) {
        let _ = self
            .view
            .evaluate_script(&format!("window.kasatermEditor?.{method}({value});"));
    }

    pub fn command(&self, name: &str, payload: serde_json::Value) {
        let name = serde_json::to_string(name).unwrap();
        let _ = self.view.evaluate_script(&format!(
            "window.kasatermEditor?.command({name},{payload});"
        ));
    }

    pub fn probe(&self, script: &str) {
        if crate::verification_run() {
            let _ = self
                .view
                .evaluate_script_with_callback(script, |result| eprintln!("[rich-probe] {result}"));
        }
    }

    #[cfg(target_os = "macos")]
    pub fn snapshot_probe(&self, path: String) {
        use wry::WebViewExtMacOS;
        if !crate::verification_run() || !std::path::Path::new(&path).is_absolute() { return; }
        let done = block2::RcBlock::new(move |image: *mut objc2_app_kit::NSImage, error: *mut objc2_foundation::NSError| {
            let result = (|| -> std::result::Result<(), String> {
                if let Some(error) = unsafe { error.as_ref() } { return Err(error.localizedDescription().to_string()); }
                let image = unsafe { image.as_ref() }.ok_or("빈 스냅샷")?;
                let tiff = image.TIFFRepresentation().ok_or("이미지 변환 실패")?;
                let bitmap = objc2_app_kit::NSBitmapImageRep::imageRepWithData(&tiff).ok_or("비트맵 변환 실패")?;
                let png = unsafe { bitmap.representationUsingType_properties(objc2_app_kit::NSBitmapImageFileType::PNG, &objc2_foundation::NSDictionary::new()) }.ok_or("PNG 변환 실패")?;
                if !png.writeToFile_atomically(&objc2_foundation::NSString::from_str(&path), true) { return Err("스냅샷 저장 실패".into()); }
                Ok(())
            })();
            eprintln!("[rich-snapshot] {path} {result:?}");
        });
        unsafe { self.view.webview().takeSnapshotWithConfiguration_completionHandler(None, &done) };
    }

    pub fn init_document(&mut self, text: String, editable: bool) {
        self.ready = true;
        let theme = crate::theme::document_tokens_json();
        self.call("init", serde_json::json!({"token":self.token,"markdown":text,"revision":self.revision,"theme":theme,"editable":editable}));
    }

    pub fn change_document(&mut self, text: String, editable: bool) {
        self.token = uuid::Uuid::new_v4().simple().to_string();
        if let Ok(mut token) = self.auth_token.lock() { *token = self.token.clone(); }
        self.revision += 1;
        self.pending = None;
        let _ = self.view.evaluate_script(&format!("window.__KASATERM_DOC_TOKEN__={};", serde_json::to_string(&self.token).unwrap()));
        self.init_document(text, editable);
    }

    pub fn replace(&mut self, text: String) {
        self.revision += 1;
        self.call(
            "setContent",
            serde_json::json!({"markdown":text,"revision":self.revision}),
        );
    }

    pub fn sync(&mut self, body: (f32, f32, f32, f32), show: bool) {
        if !show {
            if self.visible {
                let _ = self.view.set_visible(false);
                if let Some(backing) = &self.backing {
                    crate::webpane::detach_child(&self.parent, backing);
                    backing.set_visible(false);
                }
                self.visible = false;
            }
            return;
        }
        let scale = self.parent.scale_factor();
        let origin = self
            .parent
            .inner_position()
            .unwrap_or_default()
            .to_logical::<f64>(scale);
        let (x, y) = if self.backing.is_some() {
            (origin.x + body.0 as f64, origin.y + body.1 as f64)
        } else {
            (body.0 as f64, body.1 as f64)
        };
        let frame = (
            x.round() as i32,
            y.round() as i32,
            body.2.max(1.0).round() as u32,
            body.3.max(1.0).round() as u32,
        );
        if self.frame != Some(frame) {
            if let Some(backing) = &self.backing {
                let _ = backing.request_inner_size(LogicalSize::new(body.2 as f64, body.3 as f64));
                backing.set_outer_position(winit::dpi::LogicalPosition::new(x, y));
            }
            let position = if self.backing.is_some() {
                (0.0, 0.0)
            } else {
                (x, y)
            };
            let _ = self.view.set_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(position.0, position.1).into(),
                size: wry::dpi::LogicalSize::new(body.2 as f64, body.3 as f64).into(),
            });
            self.frame = Some(frame);
        }
        if !self.visible {
            if let Some(backing) = &self.backing {
                crate::webpane::attach_child(&self.parent, backing);
                backing.set_visible(true);
            }
            let _ = self.view.set_visible(true);
            self.visible = true;
        }
    }
}
