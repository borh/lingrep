let mut suffix = String::new();
suffix.push_str(&format!("g{generation}"));
suffix.push('.');
suffix.push_str(&identity_digest);
path.set_extension(format!("{suffix}.lingcorpus"));
