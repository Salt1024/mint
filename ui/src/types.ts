export interface LoginRequest {
  username: string
  password: string
}

export interface SessionInfo {
  username: string
}

export interface FileEntry {
  name: string
  path: string
  type: 'file' | 'directory'
  size: number | null
  modifiedAt: string
}

export interface DirectoryListing {
  path: string
  entries: FileEntry[]
}

export interface BatchDeleteResult {
  deleted: string[]
  failed: { path: string; message: string }[]
}
