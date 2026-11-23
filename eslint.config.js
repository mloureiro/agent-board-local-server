import js from '@eslint/js';
import tseslint from 'typescript-eslint';

export default tseslint.config(
  { ignores: ['node_modules/', 'src-tauri/', 'dist/', '.claude/', '.core/', 'wiki/'] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ['ui/**/*.js'],
    languageOptions: { globals: { window: 'readonly', document: 'readonly' } },
  },
);
