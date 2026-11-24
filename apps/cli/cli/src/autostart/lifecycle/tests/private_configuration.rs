#[test]
fn private_configuration_refuses_unknown_files_links_and_unplanned_changes() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().canonicalize()?.join("registration.json");
    private_write(&path, b"initial", None)?;
    anyhow::ensure!(private_write(&path, b"overwrite", None).is_err());
    anyhow::ensure!(private_write(&path, b"overwrite", Some(&hash(b"wrong"))).is_err());
    private_write(&path, b"updated", Some(&hash(b"initial")))?;
    anyhow::ensure!(fs::read(&path)? == b"updated");
    #[cfg(unix)]
    {
        let link = directory.path().canonicalize()?.join("link");
        std::os::unix::fs::symlink(&path, &link)?;
        anyhow::ensure!(private_write(&link, b"bad", Some(&hash(b"updated"))).is_err());
    }
    Ok(())
}
