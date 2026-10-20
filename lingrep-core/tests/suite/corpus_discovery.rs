// Exempt test/bench/example code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(
    clippy::string_slice,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::todo,
    clippy::unimplemented,
    clippy::unreachable,
    clippy::exit,
    clippy::unchecked_time_subtraction,
    clippy::unwrap_used
)]
//! Observable contract tests for bounded recursive Markdown source discovery.

use std::fs;
use std::io;
use std::os::unix::fs::symlink;

use lingrep_core::corpus::{CorpusDiscovery, CorpusFileKind, DiscoveryError, TraversalLimits};
use tempfile::tempdir;

fn limits() -> TraversalLimits {
    TraversalLimits::default()
}

/// Whether this filesystem will hold a name that is not valid UTF-8.
///
/// A Unix filename is a byte string and ext4 stores one verbatim, so discovery
/// has to cope with it and the assertions below are worth making. APFS
/// validates UTF-8 at creation and answers `EILSEQ`, so the fixture cannot be
/// built there at all — the code under test is unaffected, only the ability to
/// stage an input for it.
///
/// This is a property of the **filesystem**, not the OS: a Linux exFAT or
/// FAT32 mount refuses the same name, and a `cfg(target_os)` gate would both
/// mis-describe that and, worse, turn absent coverage into a silently green
/// run. Probing reports the skip instead, and runs the assertions anywhere
/// they can run.
fn non_utf8_names_supported(directory: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStringExt;

    let probe = directory.join(std::ffi::OsString::from_vec(b"probe-\xff".to_vec()));
    match fs::write(&probe, "") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(error) => {
            // Deliberately the `stderr()` handle and not `eprintln!`. libtest
            // captures the macro and replays it only for a *failing* test, so
            // a notice printed that way from a passing one is discarded — the
            // silently-green outcome this probe exists to avoid. Writing the
            // handle bypasses the capture and reaches the run's stderr.
            // Verified both ways before choosing.
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr(),
                "note: skipping the non-UTF-8 name assertions — this filesystem refuses such a name ({error})"
            );
            false
        }
    }
}

fn collect(
    root: &std::path::Path,
    limits: TraversalLimits,
) -> Result<lingrep_core::corpus::DiscoveryReport, DiscoveryError> {
    let mut discovery = CorpusDiscovery::new(root, limits)?;
    while discovery.next_document()?.is_some() {}
    discovery.finish()
}

fn documents(
    root: &std::path::Path,
) -> Result<
    (
        Vec<lingrep_core::corpus::SourceDocument>,
        lingrep_core::corpus::DiscoveryReport,
    ),
    DiscoveryError,
> {
    let mut discovery = CorpusDiscovery::new(root, limits())?;
    let mut documents = Vec::new();
    while let Some(document) = discovery.next_document()? {
        documents.push(document);
    }
    Ok((documents, discovery.finish()?))
}

#[test]
fn conllu_selection_reuses_bounded_recursive_order_and_ignores_other_files()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempdir()?;
    fs::write(directory.path().join("b.conllu"), "b")?;
    fs::write(directory.path().join("README.md"), "ignore")?;
    fs::create_dir(directory.path().join("split"))?;
    fs::write(directory.path().join("split/a.CONLLU"), "a")?;

    let mut discovery =
        CorpusDiscovery::new_for_kind(directory.path(), limits(), CorpusFileKind::Conllu)?;
    let mut names = Vec::new();
    while let Some(document) = discovery.next_document()? {
        names.push(document.logical_name);
    }
    let report = discovery.finish()?;

    assert_eq!(names, ["b.conllu", "split/a.CONLLU"]);
    assert_eq!(report.inventory.documents, 2);
    Ok(())
}

/// Removing recursive DFS or changing it to a global/path order breaks the
/// authored logical inventory. A hard-link remains a distinct document while
/// sharing the first target group it resolves to.
#[test]
fn recursively_discovers_markdown_in_per_directory_depth_first_order()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempdir()?;
    fs::write(directory.path().join("a.md"), "a")?;
    fs::create_dir(directory.path().join("shard"))?;
    fs::write(directory.path().join("shard/b.md"), "b")?;
    fs::write(directory.path().join("ignored.txt"), "ignore")?;
    fs::hard_link(directory.path().join("a.md"), directory.path().join("z.md"))?;

    let (documents, report) = documents(directory.path())?;
    let names = documents
        .iter()
        .map(|document| document.logical_name.as_str())
        .collect::<Vec<_>>();

    assert_eq!(names, ["a.md", "shard/b.md", "z.md"]);
    assert_eq!(report.inventory.alias_documents, 1);
    assert_eq!(
        documents[0].target_group_ordinal,
        documents[2].target_group_ordinal
    );
    Ok(())
}

/// Symlinked files and directories are admitted under their lexical names;
/// an external final target is reported rather than silently excluded.
#[test]
fn follows_symlinked_directories_and_reports_external_file_targets()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempdir()?;
    let external = tempdir()?;
    fs::create_dir(directory.path().join("shard"))?;
    fs::write(directory.path().join("shard/b.md"), "b")?;
    fs::write(external.path().join("outside.md"), "outside")?;
    symlink("shard", directory.path().join("linked-shard"))?;
    symlink(
        external.path().join("outside.md"),
        directory.path().join("external.md"),
    )?;

    let (documents, _) = documents(directory.path())?;
    let names = documents
        .iter()
        .map(|document| document.logical_name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["external.md", "linked-shard/b.md", "shard/b.md"]);
    let external = documents
        .iter()
        .find(|document| document.logical_name == "external.md")
        .expect("external file is admitted");
    assert!(external.external_target);
    Ok(())
}

/// Cycle links, dangling links, and non-UTF-8 admitted names are input errors;
/// two independent aliases to one directory are not cycles and both emit.
#[test]
fn distinguishes_cycles_dangling_links_non_utf8_names_and_non_ancestral_aliases()
-> Result<(), Box<dyn std::error::Error>> {
    let cycle = tempdir()?;
    symlink(".", cycle.path().join("latest"))?;
    assert!(matches!(
        documents(cycle.path()),
        Err(DiscoveryError::Cycle { .. })
    ));

    let dangling = tempdir()?;
    symlink("absent.md", dangling.path().join("missing.md"))?;
    let dangling_result = documents(dangling.path());
    assert!(matches!(
        dangling_result,
        Err(DiscoveryError::DanglingLink { .. })
    ));

    let invalid_name = tempdir()?;
    if non_utf8_names_supported(invalid_name.path()) {
        use std::os::unix::ffi::OsStringExt;
        fs::write(
            invalid_name
                .path()
                .join(std::ffi::OsString::from_vec(b"bad-\xff.md".to_vec())),
            "bad",
        )?;
        assert!(matches!(
            documents(invalid_name.path()),
            Err(DiscoveryError::NonUtf8Name { .. })
        ));
    }

    let aliases = tempdir()?;
    fs::create_dir(aliases.path().join("shared"))?;
    fs::write(aliases.path().join("shared/a.md"), "a")?;
    symlink("shared", aliases.path().join("first"))?;
    symlink("shared", aliases.path().join("second"))?;
    let (documents, report) = documents(aliases.path())?;
    assert_eq!(
        documents
            .iter()
            .map(|document| document.logical_name.as_str())
            .collect::<Vec<_>>(),
        ["first/a.md", "second/a.md", "shared/a.md"]
    );
    assert_eq!(report.inventory.alias_documents, 2);
    Ok(())
}

#[cfg(target_family = "unix")]
#[test]
fn ignores_non_utf8_non_markdown_file_symlinks_before_logical_name_validation()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::ffi::OsStringExt;

    let directory = tempdir()?;
    if !non_utf8_names_supported(directory.path()) {
        return Ok(());
    }
    fs::write(directory.path().join("a.md"), "a")?;
    fs::write(directory.path().join("sidecar.txt"), "ignore")?;
    symlink(
        "sidecar.txt",
        directory
            .path()
            .join(std::ffi::OsString::from_vec(b"bad-\xff.txt".to_vec())),
    )?;

    let (documents, _) = documents(directory.path())?;
    assert_eq!(
        documents
            .iter()
            .map(|document| document.logical_name.as_str())
            .collect::<Vec<_>>(),
        ["a.md"]
    );
    Ok(())
}

#[cfg(target_family = "unix")]
#[test]
fn rejects_non_utf8_markdown_and_directory_symlink_names() -> Result<(), Box<dyn std::error::Error>>
{
    use std::os::unix::ffi::OsStringExt;

    let markdown = tempdir()?;
    if !non_utf8_names_supported(markdown.path()) {
        return Ok(());
    }
    fs::write(markdown.path().join("target.md"), "target")?;
    symlink(
        "target.md",
        markdown
            .path()
            .join(std::ffi::OsString::from_vec(b"bad-\xff.md".to_vec())),
    )?;
    assert!(matches!(
        documents(markdown.path()),
        Err(DiscoveryError::NonUtf8Name { .. })
    ));

    let directory = tempdir()?;
    fs::create_dir(directory.path().join("target"))?;
    fs::write(directory.path().join("target/a.md"), "a")?;
    symlink(
        "target",
        directory
            .path()
            .join(std::ffi::OsString::from_vec(b"bad-\xff".to_vec())),
    )?;
    assert!(matches!(
        documents(directory.path()),
        Err(DiscoveryError::NonUtf8Name { .. })
    ));
    Ok(())
}

#[cfg(target_family = "unix")]
#[test]
fn discovery_failures_report_only_root_relative_logical_paths()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::ffi::OsStringExt;

    let limited = tempdir()?;
    fs::write(limited.path().join("a.md"), "a")?;
    fs::write(limited.path().join("b.md"), "b")?;
    let mut source_entries = limits();
    source_entries.max_source_entries = 1;
    let error = collect(limited.path(), source_entries).unwrap_err();
    assert!(
        !error
            .to_string()
            .contains(&limited.path().display().to_string())
    );
    assert!(error.to_string().contains(".md"), "{error}");

    let non_utf8 = tempdir()?;
    if non_utf8_names_supported(non_utf8.path()) {
        fs::write(
            non_utf8
                .path()
                .join(std::ffi::OsString::from_vec(b"bad-\xff.md".to_vec())),
            "bad",
        )?;
        let error = documents(non_utf8.path()).unwrap_err();
        assert!(
            !error
                .to_string()
                .contains(&non_utf8.path().display().to_string())
        );
        assert!(error.to_string().contains("bad-"), "{error}");
    }

    let io_failure = tempdir()?;
    symlink("b.md", io_failure.path().join("a.md"))?;
    symlink("a.md", io_failure.path().join("b.md"))?;
    let error = documents(io_failure.path()).unwrap_err();
    assert!(
        !error
            .to_string()
            .contains(&io_failure.path().display().to_string())
    );
    assert!(error.to_string().contains("a.md"), "{error}");
    Ok(())
}

/// Every traversal resource bound rejects the first value beyond its configured
/// limit and names the resource that stopped traversal.
#[test]
fn rejects_each_traversal_limit_at_the_first_excess() -> io::Result<()> {
    let directory = tempdir()?;
    fs::write(directory.path().join("a.md"), "a")?;
    fs::write(directory.path().join("b.md"), "b")?;
    fs::write(directory.path().join("c.md"), "c")?;

    let mut source_entries = limits();
    source_entries.max_source_entries = 2;
    let error = collect(directory.path(), source_entries).unwrap_err();
    assert!(matches!(
        &error,
        DiscoveryError::Limit {
            field: "max_source_entries",
            limit: 2,
            observed: 3,
            ..
        }
    ));
    assert!(
        !error
            .to_string()
            .contains(&directory.path().display().to_string())
    );

    let mut directory_entries = limits();
    directory_entries.max_directory_entries = 2;
    let error = collect(directory.path(), directory_entries).unwrap_err();
    assert!(matches!(
        &error,
        DiscoveryError::Limit {
            field: "max_directory_entries",
            limit: 2,
            observed: 3,
            ..
        }
    ));
    assert!(
        !error
            .to_string()
            .contains(&directory.path().display().to_string())
    );

    let mut documents_limit = limits();
    documents_limit.max_documents = 2;
    let error = collect(directory.path(), documents_limit).unwrap_err();
    assert!(matches!(
        &error,
        DiscoveryError::Limit {
            field: "max_documents",
            limit: 2,
            observed: 3,
            ..
        }
    ));
    assert!(
        !error
            .to_string()
            .contains(&directory.path().display().to_string())
    );

    let nested = tempdir()?;
    fs::create_dir(nested.path().join("shard"))?;
    fs::write(nested.path().join("shard/a.md"), "a")?;
    let mut depth = limits();
    depth.max_depth = 0;
    let error = collect(nested.path(), depth).unwrap_err();
    assert!(matches!(
        &error,
        DiscoveryError::Limit {
            field: "max_depth",
            limit: 0,
            observed: 1,
            ..
        }
    ));
    assert!(
        !error
            .to_string()
            .contains(&nested.path().display().to_string())
    );

    let mut index = limits();
    index.max_index_bytes = 15;
    let error = collect(directory.path(), index).unwrap_err();
    assert!(matches!(
        &error,
        DiscoveryError::Limit {
            field: "max_index_bytes",
            limit: 15,
            observed: 16,
            ..
        }
    ));
    assert!(
        !error
            .to_string()
            .contains(&directory.path().display().to_string())
    );
    Ok(())
}

/// The index limit accounts for allocation capacity, not merely the four bytes
/// in `a.md`: one retained directory-entry allocation already exceeds 64 bytes.
#[test]
fn index_limit_includes_retained_allocation_capacity() -> io::Result<()> {
    let directory = tempdir()?;
    fs::write(directory.path().join("a.md"), "a")?;

    let mut index = limits();
    index.max_index_bytes = 64;
    assert!(matches!(
        collect(directory.path(), index),
        Err(DiscoveryError::Limit {
            field: "max_index_bytes",
            limit: 64,
            observed,
            ..
        }) if observed > 64
    ));
    Ok(())
}

#[test]
fn root_index_limit_failures_use_the_logical_dot_for_files_and_directories() -> io::Result<()> {
    let directory = tempdir()?;
    fs::write(directory.path().join("a.md"), "a")?;
    let file = directory.path().join("root.md");
    fs::write(&file, "root")?;

    for root in [directory.path(), file.as_path()] {
        let mut index = limits();
        index.max_index_bytes = 0;
        let error = match CorpusDiscovery::new(root, index) {
            Ok(_) => panic!("zero root-index limit must reject {root:?}"),
            Err(error) => error,
        };

        assert!(matches!(
            &error,
            DiscoveryError::Limit {
                field: "max_index_bytes",
                logical_path,
                ..
            } if logical_path == std::path::Path::new(".")
        ));
        assert!(
            !error.to_string().contains(&root.display().to_string()),
            "{error}"
        );
    }
    Ok(())
}

#[test]
fn root_validation_is_bounded_and_does_not_enumerate_children()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempdir()?;
    fs::write(directory.path().join("a.md"), "a")?;

    let mut child_limit = limits();
    child_limit.max_directory_entries = 0;
    let validated = CorpusDiscovery::validate_root(directory.path(), child_limit.clone())?;
    assert!(validated.is_directory());
    assert!(matches!(
        CorpusDiscovery::from_validated_root(validated),
        Err(DiscoveryError::Limit {
            field: "max_directory_entries",
            ..
        })
    ));

    let mut root_limit = limits();
    root_limit.max_index_bytes = 0;
    assert!(matches!(
        CorpusDiscovery::validate_root(directory.path(), root_limit),
        Err(DiscoveryError::Limit {
            field: "max_index_bytes",
            logical_path,
            ..
        }) if logical_path == std::path::Path::new(".")
    ));
    Ok(())
}

#[test]
fn root_validation_classifies_missing_dangling_and_unsupported_roots()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::net::UnixListener;

    let directory = tempdir()?;
    let missing = directory.path().join("missing.md");
    let dangling = directory.path().join("dangling.md");
    symlink("absent.md", &dangling)?;
    let socket = directory.path().join("unsupported.sock");
    let _listener = UnixListener::bind(&socket)?;

    assert!(matches!(
        CorpusDiscovery::validate_root(&missing, limits()),
        Err(DiscoveryError::IoAt { logical_path, .. })
            if logical_path == std::path::Path::new(".")
    ));
    assert!(matches!(
        CorpusDiscovery::validate_root(&dangling, limits()),
        Err(DiscoveryError::DanglingLink { logical_path })
            if logical_path == std::path::Path::new(".")
    ));
    assert!(matches!(
        CorpusDiscovery::validate_root(&socket, limits()),
        Err(DiscoveryError::IoAt { logical_path, .. })
            if logical_path == std::path::Path::new(".")
    ));
    Ok(())
}
