import js from '@eslint/js';
import tseslint from 'typescript-eslint';

export default tseslint.config(
  { ignores: ['node_modules/', 'src-tauri/', 'dist/', '.claude/', 'wiki/'] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
);
