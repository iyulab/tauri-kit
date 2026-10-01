// This repository's own public-text check (see CONTRIBUTING.md): the default rules of
// tauri-kit-dev's public-text check, plus the traces a consumer's private workspace tends to leave in
// code it hands upstream. It deliberately names no consumer — a list of consumer names committed
// here would publish them. Each consumer checks its own names with its own config before
// contributing.
export default {
  forbidden: [
    { why: 'workspace record id', re: /\b(?:cycle|run)-\d+\b|\b[A-Z]{1,2}-\d{2,3}\b(?!-)/ },
    { why: 'process notes', re: /\bclaudedocs\b|\bHANDOFF\.md\b|\bROADMAP\.md\b/ },
    { why: 'consumer reference', re: /\bour app\b|\bthe consumer app\b|\bumbrella\b/i },
  ],
  allowed: [
    // The rules above, as written.
    'scripts/public-text.config.js:',
    // Made-up home folders of an account named "someone" — test input for the error report, to
    // show such paths are stripped. Matched by content, so a real path in the same file still counts.
    '/home/someone/',
    '/Users/someone/',
  ],
}
