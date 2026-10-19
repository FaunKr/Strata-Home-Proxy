// Lints the Node scripts under scripts/; scripts/check.sh runs it. A clean git
// merge once left a duplicate key in ui-check.js that silently dropped checks.
const node = ['require', 'module', 'process', 'console', 'setTimeout', 'clearTimeout', 'Buffer', '__dirname', 'URL', 'fetch'];
// ui-check.js runs some functions inside the page.
const browser = ['document', 'window'];

export default [
  {
    files: ['scripts/**/*.js'],
    languageOptions: {
      sourceType: 'commonjs',
      globals: Object.fromEntries([...node, ...browser].map(name => [name, 'readonly'])),
    },
    rules: {
      'no-dupe-keys': 'error',
      'no-undef': 'error',
      'no-unused-vars': 'error',
    },
  },
];
