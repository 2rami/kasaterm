use kasa_socket::agent_choice::{self,Selection};
use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) struct Binding {
    pid: u32,
    started: u64,
    pub selection: Selection,
}

impl Binding {
    pub(crate) fn new(pid: u32, selection: Selection) -> anyhow::Result<Self> {
        let started=crate::input::usage_process_birth(pid).filter(|stamp|*stamp!=0).ok_or_else(||anyhow::anyhow!("account launch process identity unavailable"))?;
        Ok(Self {pid,started,selection})
    }

    pub(crate) fn live(&self) -> bool {
        crate::input::usage_process_birth(self.pid)==Some(self.started)
    }
}

impl crate::App {
    pub(crate) fn forget_auto_accounts(&self, provider: &str) {
        self.ws.lock().unwrap().auto_accounts.retain(|_,binding|binding.selection.provider!=provider);
        for session in self.sessions.iter().flatten() {
            session.ws.lock().unwrap().auto_accounts.retain(|_,binding|binding.selection.provider!=provider);
        }
    }
}

pub(crate) fn remote_plan(plan: &Value, machine: Option<&str>) -> anyhow::Result<Value> {
    anyhow::ensure!(plan["account_selection"]==1,"대상 앱을 업데이트해야 전체 계정으로 실행할 수 있어요");
    anyhow::ensure!(plan["machine_id"].as_str()==machine,"계정을 조회한 기기가 요청한 기기와 달라요");
    agent_choice::selection(plan).map_err(anyhow::Error::msg)?;
    Ok(plan.clone())
}

/// 이어 가기인데 저장 계정이 로그아웃이면 같은 하네스의 쓸 수 있는 계정으로 잇는다 — 복원 때 학생이 「saved
/// account is logged out」 한 줄로 안 뜨고 남던 칸(2026-11-14 집 PC)을 막는다. 새로 띄우기는 고른 계정 그대로다.
pub(crate) fn resolve_or_substitute(values: &Value, selected: &Selection, resuming: bool) -> anyhow::Result<(Selection,BTreeMap<String,String>)> {
    match resolve(values,selected,resuming) {
        Ok(env) => Ok((selected.clone(),env)),
        Err(error) if resuming && agent_choice::validate_resume(values,selected)==Err("saved account is logged out") => {
            let substitute=agent_choice::resume_substitute(values,&selected.provider).ok_or(error)?;
            let env=resolve(values,&substitute,true)?;
            Ok((substitute,env))
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn resolve(values: &Value, selected: &Selection, resuming: bool) -> anyhow::Result<BTreeMap<String,String>> {
    if resuming {agent_choice::validate_resume(values,selected)} else {agent_choice::validate_selection(values,selected)}
        .map_err(anyhow::Error::msg)?;
    let mut env=BTreeMap::from([
        ("KASATERM_AUTO_ACCOUNT_PROVIDER".into(),selected.provider.clone()),
        ("KASATERM_AUTO_ACCOUNT_ID".into(),selected.account_id.clone()),
    ]);
    let dir=if selected.provider=="claude" {
        let active=crate::socket::read_claude_account();
        crate::claude_auth::runtime_dir_for(&selected.account_id,&active)
            .ok_or_else(||anyhow::anyhow!("selected Claude account storage unavailable"))?
    } else {
        crate::socket::codex_account_dir(&selected.account_id)
            .or_else(||kasa_socket::home_dir().map(|p|p.join(".codex")))
            .ok_or_else(||anyhow::anyhow!("selected Codex account storage unavailable"))?
    };
    env.insert(if selected.provider=="claude" {"CLAUDE_SECURESTORAGE_CONFIG_DIR"} else {"KASATERM_CODEX_ACCOUNT_DIR"}.into(),dir.to_string_lossy().into_owned());
    Ok(env)
}


pub(crate) fn hosts(pane: &kasa_pty::PtySession, pid: u32) -> bool {
    let Some(shell)=pane.shell_pid() else {return false};
    let table=kasa_pty::fresh_process_table();
    if let Some((_,harness))=kasa_pty::agent_pid_for_shell(&table,shell) {
        if harness!=pid {return false;}
    }
    if let Some(on_tty)=pane.hosts_process(pid) {return on_tty;}
    crate::agent_identity::descends_from(&table,pid,shell)
}


pub(crate) fn restore_command(record: &Value, machine: &str, command: &str) -> anyhow::Result<String> {
    if machine.is_empty() || record["auto_account_machine"].as_str()!=Some(machine) {return Ok(command.into());}
    let Some(value)=record.get("auto_account") else {return Ok(command.into())};
    let selected:Selection=serde_json::from_value(value.clone())?;
    let suffix=if command.ends_with('\r') {"\r"} else if command.ends_with('\n') {"\n"} else {""};
    Ok(selected.command(command.trim_end_matches(['\r','\n'])).map_err(anyhow::Error::msg)?+suffix)
}

pub(crate) fn fixture_values() -> Option<Value> {
    if !crate::verification_run() {return None;}
    let root=kasa_socket::isolated_collab_root()?;
    let path=std::path::PathBuf::from(std::env::var_os("KASATERM_TEST_ACCOUNT_CHOICES")?);
    if !path.starts_with(root) || path.components().any(|c|c==std::path::Component::ParentDir) {return None;}
    let raw=std::fs::read(path).ok()?;
    if raw.len()>128*1024 {return None;}
    serde_json::from_slice(&raw).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn recycled_process_ids_do_not_keep_an_old_account_binding() {
        let mut binding=Binding::new(std::process::id(),Selection {provider:"claude".into(),account_id:"fixture".into()}).unwrap();
        assert!(binding.live());
        binding.started=binding.started.wrapping_add(1);
        assert!(!binding.live());
    }

    #[test]
    fn remote_plan_requires_the_new_contract_and_the_requested_machine() {
        let values=json!({"account_selection":1,"machine_id":"target","provider":"claude","account_id":"standby","used_percent":1});
        assert_eq!(remote_plan(&values,Some("target")).unwrap()["account_id"],"standby");
        assert!(remote_plan(&values,Some("different")).is_err());
        let mut old=values;old["account_selection"]=json!(0);
        assert!(remote_plan(&old,Some("target")).is_err());
    }

    #[test]
    fn a_logged_out_saved_slot_resumes_on_another_slot_of_the_same_harness() {
        let values=json!({"claude":{"account":"a","codex_account":"",
            "accounts":[{"id":"a","logged_in":false,"usage_state":"logged_out"},
                {"id":"b","label":"b","logged_in":true,"usage":10.0,"usage_stale":false,"usage_state":"ready"}],
            "codex_accounts":[{"id":"","logged_in":true,"usage":0.0,"usage_stale":false,"usage_state":"ready"}]}});
        let saved=Selection {provider:"claude".into(),account_id:"a".into()};
        let substitute=|resuming|resolve_or_substitute(&values,&saved,resuming).map(|(s,_)|s);
        assert!(substitute(false).is_err(),"a fresh launch keeps the chosen slot");
        if let Ok(selected)=substitute(true) {
            assert_eq!((selected.provider.as_str(),selected.account_id.as_str()),("claude","b"));
        } else {
            assert!(crate::claude_auth::runtime_dir_for("b",&crate::socket::read_claude_account()).is_none(),"only storage may refuse the stand-in");
        }
    }

    #[test]
    fn saved_slot_is_restored_only_on_its_original_machine() {
        let record=json!({"auto_account":{"provider":"claude","account_id":"acct-2"},"auto_account_machine":"source"});
        assert_eq!(restore_command(&record,"source","claude --resume previous\r").unwrap(),"kasaterm-cli account-run claude 'acct-2' -- claude --resume previous\r");
        assert_eq!(restore_command(&record,"destination","claude --resume previous\r").unwrap(),"claude --resume previous\r");
        let invalid=json!({"auto_account":{"provider":"claude","account_id":"../bad"},"auto_account_machine":"source"});
        assert!(restore_command(&invalid,"source","claude").is_err());
    }
}
