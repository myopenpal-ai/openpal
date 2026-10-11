//! Stage `LICENSE` and a generated `NOTICE` into a Portable bundle.
//!
//! `LICENSE` is OpenPal's own (MIT). `NOTICE` attributes the third-party code
//! every Portable build redistributes — the llama.cpp engine (always), the
//! fixed-version WebView2 runtime (Windows), and the pre-staged model and its
//! license when `--model` was used. Shipping these is what makes redistribution
//! of the bundle clean.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use crate::model::CatalogModel;
use crate::paths;

/// MIT text used if the repo's own `LICENSE` file is somehow missing.
const MIT_FALLBACK: &str = "\
MIT License

Copyright (c) myopenpal.ai

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the \"Software\"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
";

/// Copy OpenPal's `LICENSE` into the bundle root (falling back to embedded MIT
/// text if the repo file is missing).
pub fn write_license(stage_dir: &Path) -> Result<()> {
    let dest = stage_dir.join("LICENSE");
    let repo_license = paths::repo_root().join("LICENSE");
    if repo_license.is_file() {
        fs::copy(&repo_license, &dest)
            .with_context(|| format!("failed to copy LICENSE into {}", stage_dir.display()))?;
    } else {
        fs::write(&dest, MIT_FALLBACK)
            .with_context(|| format!("failed to write LICENSE into {}", stage_dir.display()))?;
    }
    Ok(())
}

/// Write the third-party `NOTICE` for a bundle built for `target`, listing the
/// pre-staged `model` when one was embedded.
pub fn write_notice(stage_dir: &Path, target: &str, model: Option<&CatalogModel>) -> Result<()> {
    let mut n = String::new();
    n.push_str("OpenPal — Portable distribution\n");
    n.push_str("Copyright (c) myopenpal.ai. Licensed under the MIT License (see LICENSE).\n\n");
    n.push_str("This distribution redistributes the following third-party components:\n\n");

    n.push_str("• llama.cpp (the local inference engine, in `llama/` or inside the AppImage)\n");
    n.push_str("  Copyright (c) 2023-2024 The ggml authors. MIT License.\n");
    n.push_str("  https://github.com/ggml-org/llama.cpp\n\n");

    if target.contains("win") {
        n.push_str("• Microsoft Edge WebView2 fixed-version runtime (`webview2-runtime/`)\n");
        n.push_str("  © Microsoft Corporation. Redistributed under the Microsoft Software\n");
        n.push_str("  License Terms for the WebView2 Runtime; see Microsoft's terms.\n");
        n.push_str("  https://developer.microsoft.com/microsoft-edge/webview2/\n\n");
    }

    if let Some(m) = model {
        n.push_str(&format!(
            "• Model weights: {} (`models/{}`)\n",
            m.name, m.file_name
        ));
        n.push_str(&format!(
            "  Publisher: {}. License: {}.\n",
            m.publisher, m.license
        ));
        n.push_str(&format!("  Source: {}\n", m.url));
        if !crate::model::is_permissive_license(&m.license) {
            n.push_str(
                "  NOTE: this license imposes pass-through obligations on redistribution \
                 (e.g. shipping the agreement / attribution). Ensure compliance.\n",
            );
        }
        n.push('\n');
    }

    let dest = stage_dir.join("NOTICE");
    fs::write(&dest, n)
        .with_context(|| format!("failed to write NOTICE into {}", stage_dir.display()))?;
    Ok(())
}
