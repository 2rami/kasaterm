use super::*;

pub(crate) struct TabIdentity(u64);
impl Default for TabIdentity {
    fn default() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LocalScope {
    workspace: std::sync::Weak<std::sync::Mutex<Workspace>>,
    _sessions: Vec<std::sync::Weak<kasa_pty::PtySession>>,
    cells: Vec<Vec<String>>,
    remote: Vec<String>,
}

impl PartialEq for LocalScope {
    fn eq(&self, other: &Self) -> bool {
        self.workspace.ptr_eq(&other.workspace) && self.cells == other.cells && self.remote == other.remote
    }
}
impl Eq for LocalScope {}
impl LocalScope {
    pub(super) fn matches_workspace(&self, app: &App) -> bool {
        self.workspace.ptr_eq(&Arc::downgrade(&app.ws))
    }
}

fn pty_token(app: &App, id: &str) -> String {
    let generation = app.pty.get(id).map_or(0, |pty| Arc::as_ptr(pty) as usize);
    format!("pty:{}:{generation}", kasa_mcp::surface_keys::ensure(id))
}

fn cell_tokens(app: &App, ws: &Workspace, pane: &str) -> Vec<String> {
    let pty = |id: &str| pty_token(app, id);
    ws.panes.get(pane).map(|p| p.tabs.iter().map(|tab| tab.pid.as_deref()
        .map(&pty).unwrap_or_else(|| format!("tab:{}", tab.close_identity.0))).collect())
        .unwrap_or_else(|| vec![pty(pane)])
}

impl App {
    pub(super) fn local_close_scope(&self, action: &PendingClose, targets: &[MirrorTarget]) -> LocalScope {
        let ws = self.ws.lock().unwrap();
        let cells = match action {
            PendingClose::Tab { pane, idx } => cell_tokens(self, &ws, pane).get(*idx)
                .map(|token| vec![vec![token.clone()]]).unwrap_or_default(),
            PendingClose::Pane { pane } => vec![cell_tokens(self, &ws, pane)],
            PendingClose::Session(window) => self.window_leaves(*window).iter().map(|p| cell_tokens(self, &ws, p)).collect(),
            _ => vec![],
        };
        LocalScope {
            workspace: Arc::downgrade(&self.ws), cells,
            // Weak references prevent address reuse without extending the lifetime of a closed PTY.
            _sessions: self.pty.values().map(Arc::downgrade).collect(),
            remote: targets.iter().filter(|t| !t.local.is_empty()).map(|t| pty_token(self, &t.local)).collect(),
        }
    }

    pub(super) fn finish_local_close_scope(&mut self, action: &PendingClose, scope: &LocalScope) -> Result<()> {
        anyhow::ensure!(scope.matches_workspace(self), "작업 공간이 바뀌어 로컬 칸은 남겼어요");
        let expected: Vec<Vec<String>> = scope.cells.iter().map(|cell| cell.iter()
            .filter(|token| !scope.remote.contains(token)).cloned().collect::<Vec<_>>())
            .filter(|cell| !cell.is_empty()).collect();
        if expected.is_empty() { return Ok(()); }
        let ws = self.ws.lock().unwrap();
        let mut visible = Vec::new();
        for window in 0..self.windows.len() { visible.extend(self.window_leaves(window)); }
        visible.extend(self.aux.terminals.iter().map(|aux| aux.pane_id.clone()));
        visible.sort(); visible.dedup();
        let current: Vec<_> = visible.iter().map(|pane| (pane.clone(), cell_tokens(self, &ws, pane))).collect();
        let mut matched = Vec::new();
        for cell in &expected {
            let found: Vec<_> = current.iter().filter(|(_, tokens)| tokens.iter().any(|t| cell.contains(t))).collect();
            if found.is_empty() { continue; }
            anyhow::ensure!(found.len() == 1, "칸 구성이 바뀌어 로컬 탭은 남겼어요");
            let (pane, tokens) = found[0];
            if matches!(action, PendingClose::Tab { .. }) {
                let idx = tokens.iter().position(|t| t == &cell[0]).unwrap();
                matched.push(PendingClose::Tab { pane: pane.clone(), idx });
            } else {
                anyhow::ensure!(same_members(tokens, cell), "탭 구성이 바뀌어 로컬 칸은 남겼어요");
                matched.push(PendingClose::Pane { pane: pane.clone() });
            }
        }
        drop(ws);
        if matched.is_empty() { return Ok(()); }
        if matches!(action, PendingClose::Session(_)) {
            let panes: Vec<_> = matched.iter().filter_map(|a| match a { PendingClose::Pane { pane } => Some(pane.clone()), _ => None }).collect();
            let window = self.window_of_pane(&panes[0]).ok_or_else(|| anyhow::anyhow!("방 구성이 바뀌어 로컬 칸은 남겼어요"))?;
            anyhow::ensure!(panes.iter().all(|p| self.window_of_pane(p) == Some(window))
                && same_members(&self.window_leaves(window), &panes), "방 구성이 바뀌어 로컬 칸은 남겼어요");
            matched = vec![PendingClose::Session(window)];
        }
        anyhow::ensure!(matched.iter().all(|a| self.dirty_docs(a).is_empty()), "저장하지 않은 로컬 문서는 남겼어요");
        for action in matched { self.do_close_locally(action); }
        Ok(())
    }
}
