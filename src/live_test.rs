//! Opt-in live checks. Creates a uniquely named remote folder and never touches existing mappings.
use crate::{
    engine::Engine,
    local,
    model::*,
    remote::{Adapter, Remote},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{fs, path::Path};

pub fn run(engine: &mut Engine<Adapter>, data: &Path) -> Result<Value> {
    let login = engine.remote.call("auth.status", json!({}))?;
    ensure!(
        login["signed_in"] == true,
        "Sign in before running the live test"
    );
    let run_id = uuid::Uuid::new_v4().to_string();
    let name = format!("Peedrive integration test {}", &run_id[..8]);
    let myfiles = engine.remote.call("browse", json!({}))?;
    let root = engine.remote.mkdir(
        myfiles["uid"].as_str().context("Missing My files root")?,
        &name,
    )?;
    let test_dir = data.join("live-tests").join(&run_id);
    local::private_dir(&test_dir)?;
    let folder = test_dir.join("local");
    local::private_dir(&folder)?;
    let (root_device, root_inode) = local::root_identity(&folder)?;
    let mapping = Mapping {
        id: format!("live-{run_id}"),
        local: folder.to_str().context("Invalid test path")?.into(),
        remote: root.uid.clone(),
        remote_label: name.clone(),
        root_device,
        root_inode,
        paused: true,
        approved: true,
        ignores: vec![],
        selection: None,
    };
    // Retain the mapping paused for inspection, including when a check fails.
    engine.store.save_mapping(&mapping)?;
    engine
        .stop
        .store(false, std::sync::atomic::Ordering::Relaxed);
    let result = (|| -> Result<Value> {
        fs::write(
            folder.join("hello.txt"),
            b"Peedrive live test\nUTF-8: caf\xc3\xa9\n",
        )?;
        let plan = engine.prepare(&mapping)?;
        engine.execute(&mapping, &plan)?;
        let remote = engine
            .remote
            .inspect(&mapping.remote, "hello.txt")?
            .context("Uploaded file is missing")?;
        let verified = test_dir.join("verified-download.txt");
        engine.remote.download("hello.txt", &remote, &verified)?;
        ensure!(
            fs::read(&verified)? == fs::read(folder.join("hello.txt"))?,
            "Live download bytes differ from upload"
        );
        let edited = test_dir.join("remote-edit.txt");
        fs::write(&edited, b"Edit from the remote side\n")?;
        let hash = local::entry(&edited)?.unwrap().hash.unwrap();
        engine
            .remote
            .upload(&mapping.remote, "hello.txt", &edited, &Some(remote), &hash)?;
        fs::write(folder.join("hello.txt"), b"Simultaneous local edit\n")?;
        let plan = engine.prepare(&mapping)?;
        ensure!(
            plan.conflicts == 1,
            "Expected a live simultaneous-edit conflict"
        );
        engine.execute(&mapping, &plan)?;
        ensure!(
            fs::read(folder.join("hello.txt"))? == b"Edit from the remote side\n",
            "Remote edit was not downloaded"
        );
        let files = local::scan(&mapping)?;
        let conflict = files
            .keys()
            .find(|p| p.contains("conflict"))
            .context("Conflict copy was not preserved")?;
        ensure!(
            fs::read(folder.join(conflict))? == b"Simultaneous local edit\n",
            "Local conflict content changed"
        );
        let plan = engine.prepare(&mapping)?;
        engine.execute(&mapping, &plan)?;
        fs::write(
            folder.join("delete-me.txt"),
            b"Safe to trash: generated live test file\n",
        )?;
        let plan = engine.prepare(&mapping)?;
        engine.execute(&mapping, &plan)?;
        fs::remove_file(folder.join("delete-me.txt"))?;
        let plan = engine.prepare(&mapping)?;
        engine.execute(&mapping, &plan)?;
        ensure!(
            engine
                .remote
                .inspect(&mapping.remote, "delete-me.txt")?
                .is_none(),
            "Generated deletion did not reach Drive Trash"
        );
        let report = json!({"passed":true,"checks":["authenticated session available","SDK upload","verified download","remote revision upload","simultaneous-edit conflict preservation","conflict-copy upload","local deletion to remote trash"],"local_test_folder":folder,"remote_test_folder":name,"mapping_id":mapping.id,"mapping_paused":true});
        fs::write(
            test_dir.join("report.json"),
            serde_json::to_vec_pretty(&report)?,
        )?;
        Ok(report)
    })();
    if let Err(error) = &result {
        fs::write(
            test_dir.join("report.json"),
            serde_json::to_vec_pretty(
                &json!({"passed":false,"error":format!("{error:#}"),"remote_test_folder":name,"mapping_id":mapping.id}),
            )?,
        )?;
    }
    result
}
