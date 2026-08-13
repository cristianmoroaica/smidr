export {};

declare global {
  interface Window {
    smidrDesktop?: {
      exportProject(projectId: string): Promise<
        | { canceled: true }
        | { canceled: false; dir: string; files: string[] }
      >;
      openExportFolder(): Promise<{ path: string }>;
    };
  }
}
