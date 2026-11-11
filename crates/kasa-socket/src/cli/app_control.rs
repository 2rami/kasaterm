use super::*;
use crate::app_control::{Action, Request as Control};

pub(super) fn parse(args: &[String]) -> Result<(Control, Option<String>)> {
    let action = match args.first().map(String::as_str) {
        Some("inspect") => Action::Inspect, Some("screen") => Action::Screen,
        Some("restore") => Action::Restore, Some("restart") => Action::Restart,
        Some("update-check") => Action::UpdateCheck,
        _ => return Err(anyhow!("app inspect|screen|restore|restart|update-check [--machine ID] [--expect REVISION] [--interrupt] [--display ID] [--out FILE]")),
    };
    let mut request = Control {action,machine_id:None,expect:None,interrupt:false,display:None};
    let mut out = None;
    let mut args = args[1..].iter();
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--interrupt" if !request.interrupt => request.interrupt = true,
            "--machine" if request.machine_id.is_none() => request.machine_id = Some(args.next().ok_or_else(||anyhow!("--machine requires a stable machine ID"))?.clone()),
            "--expect" if request.expect.is_none() => request.expect = Some(args.next().ok_or_else(||anyhow!("--expect requires inspect revision"))?.clone()),
            "--display" if request.display.is_none() => request.display = Some(args.next().ok_or_else(||anyhow!("--display requires an ID"))?.parse()?),
            "--out" if out.is_none() => out = Some(args.next().ok_or_else(||anyhow!("--out requires a path"))?.clone()),
            _ => return Err(anyhow!("unknown or duplicate app option {flag}")),
        }
    }
    request.validate()?;
    if out.is_some() && action != Action::Screen { return Err(anyhow!("--out is only valid for screen")); }
    Ok((request,out))
}

pub(super) fn run(args: &[String]) -> Result<Option<Response>> {
    let (control,path) = parse(args)?;
    let req = Request {id:json!(format!("app-{}",std::process::id())),method:"app.control".into(),params:serde_json::to_value(&control)?};
    let mut response=roundtrip(&resolve_socket_path()?,&req)?;
    if control.action == Action::Screen && response.ok {
        let result=response.result.as_mut().ok_or_else(||anyhow!("missing screenshot response"))?;
        save_screen(result,path.as_deref())?;
    }
    Ok(Some(response))
}

fn save_screen(result: &mut Value, path: Option<&str>) -> Result<()> {
    use base64::Engine;
    let encoded=result["image_base64"].as_str().ok_or_else(||anyhow!("missing screenshot bytes"))?;
    let bytes=base64::engine::general_purpose::STANDARD.decode(encoded)?;
    if !bytes.starts_with(&[0xff,0xd8,0xff]) { return Err(anyhow!("screenshot is not JPEG")); }
    let path=path.map(std::path::PathBuf::from).unwrap_or_else(||std::env::temp_dir().join(format!("kasaterm-screen-{}-{}.jpg",std::process::id(),crate::board::now_ms())));
    let mut options=std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
    let mut file=options.open(&path).context("create private screenshot; existing files are not overwritten")?;
    file.write_all(&bytes)?;
    result.as_object_mut().ok_or_else(||anyhow!("invalid screenshot response"))?.remove("image_base64");
    result["path"]=json!(path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(words:&[&str])->Vec<String>{words.iter().map(|s|s.to_string()).collect()}
    #[test]
    fn mutations_require_the_observed_revision_and_strict_flags() {
        assert!(parse(&args(&["restart"])).is_err());
        assert!(parse(&args(&["restart","--expect","1.hash","--interrupt","--machine","registered"])).is_ok());
        for words in [&["inspect","--interrupt"][..], &["restore","--expect","1.hash","--out","file"],
            &["screen","--display","bad"], &["inspect","--machine","a","--machine","b"]] { assert!(parse(&args(words)).is_err()); }
    }
}
