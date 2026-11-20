pub use lingrep_cli_common::packaged_dictionaries::{
    DEFAULT, DICTIONARIES, DictionaryName, Installed, MISSING_DICTIONARY_HINT, PackagedDictionary,
    fallback_dictionary_path, install_directory,
};

pub fn install(
    dictionary: PackagedDictionary,
    directory: &std::path::Path,
    download_base: &str,
) -> Result<Installed, crate::CliError> {
    lingrep_cli_common::packaged_dictionaries::install(dictionary, directory, download_base)
        .map_err(|error| crate::CliError::Dictionary(error.to_string()))
}
