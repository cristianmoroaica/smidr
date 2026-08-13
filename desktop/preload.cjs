const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('smidrDesktop', Object.freeze({
  exportProject(projectId) {
    return ipcRenderer.invoke('smidr:export-project', projectId);
  },
  openExportFolder() {
    return ipcRenderer.invoke('smidr:open-export-folder');
  }
}));
