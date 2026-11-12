use super::*;
use crate::agent_choice::Selection;
use std::collections::BTreeMap;
use std::process::Command;

fn parse(args: &[String]) -> Result<(Selection,&[String])> {
    let [provider,id,separator,command @ ..]=args else { return Err(anyhow!("account-run PROVIDER ACCOUNT -- COMMAND…")) };
    let selected=Selection {provider:provider.clone(),account_id:if id=="@default" {String::new()} else {id.clone()}};
    selected.validate().map_err(anyhow::Error::msg)?;
    if separator!="--" || command.is_empty() { return Err(anyhow!("account-run requires -- and a harness command")); }
    let name=Path::new(&command[0]).file_stem().and_then(|s|s.to_str()).unwrap_or("");
    if command[0]!=selected.provider && name!="kasa-acp" { return Err(anyhow!("account-run only launches the selected harness or kasa-acp")); }
    if name=="kasa-acp" && !command.windows(2).any(|pair|pair[0]=="--harness" && pair[1]==selected.provider) {
        return Err(anyhow!("ACP harness differs from selected account"));
    }
    Ok((selected,command))
}

fn checked_env(value: &Value, selected: &Selection) -> Result<BTreeMap<String,String>> {
    if value["provider"]!=selected.provider || value["account_id"]!=selected.account_id { return Err(anyhow!("account selection changed")); }
    let env: BTreeMap<String,String>=serde_json::from_value(value["env"].clone()).context("invalid account environment")?;
    let account_key=if selected.provider=="claude" {"CLAUDE_SECURESTORAGE_CONFIG_DIR"} else {"KASATERM_CODEX_ACCOUNT_DIR"};
    if env.len()!=3 || !env.contains_key(account_key)
        || env.get("KASATERM_AUTO_ACCOUNT_PROVIDER")!=Some(&selected.provider)
        || env.get("KASATERM_AUTO_ACCOUNT_ID")!=Some(&selected.account_id)
        || env.values().any(|v|v.chars().any(char::is_control)) {
        return Err(anyhow!("unexpected account environment"));
    }
    Ok(env)
}

fn shim_program(shim: &Path, name: &str) -> std::path::PathBuf {
    shim.join(if cfg!(windows) {format!("{name}.cmd")} else {name.into()})
}

pub(super) fn run(args: &[String]) -> Result<Option<Response>> {
    let (selected,args)=parse(args)?;
    let overrides: &[&str] = if selected.provider=="claude" {
        &["ANTHROPIC_API_KEY","ANTHROPIC_AUTH_TOKEN","CLAUDE_CODE_OAUTH_TOKEN","CLAUDE_CODE_USE_BEDROCK","CLAUDE_CODE_USE_VERTEX","CLAUDE_CODE_USE_FOUNDRY"]
    } else { &["OPENAI_API_KEY","CODEX_API_KEY"] };
    if overrides.iter().any(|key|std::env::var_os(key).is_some_and(|v|!v.is_empty())) {
        return Err(anyhow!("explicit API credentials override subscription account selection; use an explicit launch"));
    }
    let pane=std::env::var("KASATERM_PANE_ID").context("account launch requires a pane")?;
    let resuming=(selected.provider=="claude" && args.iter().any(|arg|arg=="--resume" || arg.starts_with("--resume=")))
        || (selected.provider=="codex" && args.get(1).is_some_and(|s|s=="resume"));
    let reply=roundtrip(&resolve_socket_path()?, &Request {id:json!("account-run"),method:"agent.account_env".into(),
        params:json!({"surface_id":pane,"pid":std::process::id(),"selection":selected,"resuming":resuming})})?;
    if !reply.ok { return Err(anyhow!("{}",reply.error.map(|e|e.message).unwrap_or_else(||"selected account unavailable".into()))); }
    let mut env=checked_env(&reply.result.unwrap_or_default(),&selected)?;
    let shim=std::env::var_os("KASATERM_TMUX_SHIM_DIR").map(std::path::PathBuf::from).context("account launch requires the pane launcher")?;
    let mut program=Path::new(&args[0]).to_path_buf();
    if args[0]==selected.provider {
        program=shim_program(&shim,&selected.provider);
        if !program.is_file() { return Err(anyhow!("selected harness launcher unavailable")); }
    } else if selected.provider=="codex" {
        // ACP skips the TUI shim, but must share its linked auth, history and copied user config.
        let output=crate::process::no_window_command(shim_program(&shim,"codex")).arg("--kasaterm-home")
            .envs(&env).output().context("prepare selected Codex home")?;
        if !output.status.success() { return Err(anyhow!("selected Codex home could not be prepared")); }
        let home=String::from_utf8(output.stdout).context("invalid Codex home response")?;
        let home=home.trim();
        if home.is_empty() || home.chars().any(char::is_control) { return Err(anyhow!("invalid Codex home response")); }
        env.insert("CODEX_HOME".into(),home.into());
        let mut config: Value=match std::env::var("CODEX_CONFIG") {Ok(s)=>serde_json::from_str(&s).context("invalid CODEX_CONFIG")?,Err(_)=>json!({})};
        config.as_object_mut().ok_or_else(||anyhow!("CODEX_CONFIG must be an object"))?.insert("cli_auth_credentials_store".into(),json!("file"));
        env.insert("CODEX_CONFIG".into(),config.to_string());
    }
    let mut command=Command::new(program);
    command.args(&args[1..]).envs(env);
    #[cfg(unix)] {
        use std::os::unix::process::CommandExt;
        Err(command.exec()).context("launch selected account")
    }
    #[cfg(not(unix))] {
        let exit=command.status().context("launch selected account")?;
        std::process::exit(exit.code().unwrap_or(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_account_survives_shells_that_drop_empty_arguments() {
        let selected=Selection{provider:"codex".into(),account_id:String::new()};
        let command=selected.command("codex").unwrap();
        let words=crate::shell_line::split_posix_words(&command).unwrap();
        assert_eq!(words[3],"@default");
        assert_eq!(parse(&words[2..]).unwrap().0,selected);
        for shell in [crate::shell_line::ShellKind::Cmd,crate::shell_line::ShellKind::PowerShell] {
            let line=crate::shell_line::launch_line(shell,None,&command,None,None).unwrap();
            assert!(line.contains("@default") && !line.contains("''"));
        }
    }

    #[test]
    fn environment_cannot_change_provider_or_inject_credentials() {
        let selection=Selection{provider:"claude".into(),account_id:"acct-1".into()};
        let mut v=json!({"provider":"claude","account_id":"acct-1","env":{"KASATERM_AUTO_ACCOUNT_PROVIDER":"claude","KASATERM_AUTO_ACCOUNT_ID":"acct-1","CLAUDE_SECURESTORAGE_CONFIG_DIR":"/fixture/account"}});
        assert_eq!(checked_env(&v,&selection).unwrap()["CLAUDE_SECURESTORAGE_CONFIG_DIR"],"/fixture/account");
        v["env"]["ANTHROPIC_API_KEY"]=json!("must-not-be-accepted");
        assert!(checked_env(&v,&selection).is_err());
        v["account_id"]=json!("other");
        assert!(checked_env(&v,&selection).is_err());
    }
}
