/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** "1" serves fixtures/sample-report.json instead of calling Tauri commands. */
  readonly VITE_MOCK?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
