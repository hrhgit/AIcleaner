#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::env;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::sync::Arc;
    use std::thread::{self, JoinHandle};
    use std::time::Duration;
    use uuid::Uuid;

    mod support;

    mod chat_completion;
    mod classification;
    mod core_contract;
    mod inventory_and_directory;
    mod real_model;
    mod summary_tests;
}
