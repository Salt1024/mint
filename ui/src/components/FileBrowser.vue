<script setup lang="ts">
import type { BatchDeleteResult, DirectoryListing, FileEntry } from '@/types'
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { AlertCircle, Check, Download, File, Folder, FolderOpen, LoaderCircle, LogOut, RefreshCw, Search, Trash2 } from 'lucide-vue-next'
import { toast } from 'vue-sonner'
import { api, ApiError } from '@/api'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { AlertDialog, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from '@/components/ui/alert-dialog'
import { Badge } from '@/components/ui/badge'
import { Breadcrumb, BreadcrumbItem, BreadcrumbLink, BreadcrumbList, BreadcrumbPage, BreadcrumbSeparator } from '@/components/ui/breadcrumb'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'

defineProps<{ username: string; loggingOut: boolean }>()
const emit = defineEmits<{ logout: []; unauthorized: [] }>()
const currentPath = ref('')
const entries = ref<FileEntry[]>([])
const filter = ref('')
const loading = ref(true)
const error = ref('')
const selected = ref(new Set<string>())
const downloaded = ref(new Set<string>())
const confirmingDelete = ref(false)
const deleting = ref(false)
const archiveForm = ref<HTMLFormElement>()
const selectionLimit = 1000
const previewExtensions = new Set([
  'jpg', 'jpeg', 'png', 'gif', 'webp', 'avif', 'bmp', 'ico',
  'mp4', 'webm', 'ogv', 'mov', 'm4v',
  'mp3', 'wav', 'ogg', 'oga', 'm4a', 'aac', 'flac', 'opus',
])
let currentRequest: AbortController | undefined

const breadcrumbs = computed(() => {
  const segments = currentPath.value.split('/').filter(Boolean)
  return segments.map((name, index) => ({ name, path: segments.slice(0, index + 1).join('/') }))
})
const visibleEntries = computed(() => {
  const query = filter.value.trim().toLocaleLowerCase()
  return entries.value.filter(entry => entry.name.toLocaleLowerCase().includes(query))
})
const selectedEntries = computed(() => entries.value.filter(entry => selected.value.has(entry.path)))
const selectedDirectories = computed(() => selectedEntries.value.filter(entry => entry.type === 'directory').length)
const selectionState = computed<boolean | 'indeterminate'>(() => {
  const count = visibleEntries.value.filter(entry => selected.value.has(entry.path)).length
  return count && count === visibleEntries.value.length ? true : count ? 'indeterminate' : false
})

async function loadDirectory(path = currentPath.value, historyMode: 'push' | 'replace' = 'push') {
  currentRequest?.abort()
  const controller = new AbortController()
  currentRequest = controller
  if (path !== currentPath.value) {
    filter.value = ''
    selected.value.clear()
  }
  currentPath.value = path
  loading.value = true
  error.value = ''
  entries.value = []
  try {
    const listing = await api<DirectoryListing>(`/files?${new URLSearchParams({ path })}`, { signal: controller.signal })
    if (controller.signal.aborted) return
    currentPath.value = listing.path
    entries.value = listing.entries
    selected.value = new Set(listing.entries.filter(entry => selected.value.has(entry.path)).map(entry => entry.path))
    const url = new URL(window.location.href)
    if (listing.path) url.searchParams.set('path', listing.path)
    else url.searchParams.delete('path')
    if (url.href !== window.location.href) {
      if (historyMode === 'push') window.history.pushState(null, '', url)
      else window.history.replaceState(window.history.state, '', url)
    }
  } catch (cause) {
    if (controller.signal.aborted) return
    if (cause instanceof ApiError && cause.status === 401) emit('unauthorized')
    else error.value = (cause as Error).message
  } finally {
    if (currentRequest === controller) loading.value = false
  }
}

function selectEntry(path: string, checked: boolean | 'indeterminate') {
  if (checked === true) selected.value.add(path)
  else selected.value.delete(path)
}

function selectVisible(checked: boolean | 'indeterminate') {
  for (const entry of visibleEntries.value) selectEntry(entry.path, checked)
}

async function removeSelected() {
  if (!selected.value.size || deleting.value) return
  deleting.value = true
  try {
    const result = await api<BatchDeleteResult>('/files/delete', {
      method: 'POST', body: JSON.stringify({ paths: [...selected.value] }),
    })
    for (const path of result.deleted) {
      selected.value.delete(path)
      for (const marked of downloaded.value) {
        if (marked === path || marked.startsWith(`${path}/`)) downloaded.value.delete(marked)
      }
    }
    selected.value = new Set(result.failed.map(item => item.path))
    confirmingDelete.value = false
    if (result.deleted.length) toast.success(`已删除 ${result.deleted.length} 项`)
    if (result.failed.length) toast.error(`${result.failed.length} 项删除失败：${result.failed[0]!.message}`)
    await loadDirectory()
  } catch (cause) {
    if (cause instanceof ApiError && cause.status === 401) emit('unauthorized')
    else toast.error((cause as Error).message)
  } finally {
    deleting.value = false
  }
}

function formatSize(size: number | null) {
  if (size === null) return '—'
  if (size === 0) return '0 B'
  const unit = Math.min(Math.floor(Math.log(size) / Math.log(1024)), 4)
  return `${new Intl.NumberFormat('zh-CN', { maximumFractionDigits: 1 }).format(size / 1024 ** unit)} ${['B', 'KB', 'MB', 'GB', 'TB'][unit]}`
}

function fileUrl(action: 'download' | 'preview', path: string) {
  return `/api/files/${action}?${new URLSearchParams({ path })}`
}

function canPreview(entry: FileEntry) {
  const dot = entry.name.lastIndexOf('.')
  return entry.type === 'file' && dot > 0 && previewExtensions.has(entry.name.slice(dot + 1).toLowerCase())
}

function markDownloaded(path: string) {
  downloaded.value.add(path)
}

function downloadSelected() {
  if (!selected.value.size || selected.value.size > selectionLimit) return
  archiveForm.value?.submit()
  for (const path of selected.value) markDownloaded(path)
}

function archiveLoaded(event: Event) {
  const document = (event.target as HTMLIFrameElement).contentDocument
  if (document?.contentType !== 'application/json') return
  try {
    const body = JSON.parse(document.body.textContent ?? '') as { statusCode: number; message: string }
    if (body.statusCode === 401) emit('unauthorized')
    else toast.error(body.message)
  } catch {
    toast.error('打包下载失败，请重试')
  }
}

function loadFromUrl() {
  loadDirectory(new URL(window.location.href).searchParams.get('path') ?? '', 'replace')
}

onMounted(() => {
  window.addEventListener('popstate', loadFromUrl)
  loadFromUrl()
})
onUnmounted(() => {
  window.removeEventListener('popstate', loadFromUrl)
  currentRequest?.abort()
})
</script>

<template>
  <div class="flex min-h-screen flex-col">
    <header class="border-b">
      <div class="mx-auto flex h-16 max-w-screen-2xl items-center justify-between px-6">
        <div class="flex items-center gap-3 font-semibold">
          <FolderOpen class="size-5" />
          <h1>Mint</h1>
        </div>
        <div class="flex items-center gap-4">
          <span class="text-sm text-muted-foreground">{{ username }}</span>
          <Button variant="outline" size="sm" :disabled="loggingOut || deleting" @click="emit('logout')">
            <LoaderCircle v-if="loggingOut" class="animate-spin" />
            <LogOut v-else />
            退出
          </Button>
        </div>
      </div>
    </header>

    <main class="mx-auto flex w-full max-w-screen-2xl flex-1 flex-col gap-3 p-6">
      <div class="flex items-center gap-4">
        <div v-if="selected.size" class="flex shrink-0 items-center gap-2">
          <Button variant="outline" :disabled="loading || deleting || selected.size > selectionLimit" @click="downloadSelected"><Download />打包下载</Button>
          <Button variant="destructive" :disabled="loading || deleting || selected.size > selectionLimit" @click="confirmingDelete = true"><Trash2 />删除</Button>
          <span class="text-sm" :class="{ 'text-destructive': selected.size > selectionLimit }" :title="selected.size > selectionLimit ? `一次最多操作 ${selectionLimit} 项` : undefined">已选择 {{ selected.size }} 项</span>
          <Button variant="ghost" size="sm" :disabled="loading || deleting" @click="selected.clear()">取消选择</Button>
        </div>
        <div class="ml-auto flex shrink-0 items-center gap-2">
          <div class="relative">
            <Search class="pointer-events-none absolute left-3 top-2.5 size-4 text-muted-foreground" />
            <Input v-model="filter" class="w-64 pl-9" placeholder="筛选当前目录文件名" aria-label="筛选当前目录文件名" :disabled="loading || deleting" />
          </div>
          <Button variant="outline" :disabled="loading || deleting" @click="loadDirectory()">
            <RefreshCw :class="{ 'animate-spin': loading }" />
            刷新
          </Button>
        </div>
      </div>

      <Breadcrumb class="min-w-0 overflow-x-auto">
        <BreadcrumbList class="flex-nowrap whitespace-nowrap">
          <BreadcrumbItem class="shrink-0">
            <BreadcrumbPage v-if="!currentPath">根目录</BreadcrumbPage>
            <BreadcrumbLink v-else as-child>
              <button type="button" :disabled="deleting" @click="loadDirectory('')">根目录</button>
            </BreadcrumbLink>
          </BreadcrumbItem>
          <template v-for="(item, index) in breadcrumbs" :key="item.path">
            <BreadcrumbSeparator />
            <BreadcrumbItem class="min-w-0 shrink-0">
              <BreadcrumbPage v-if="index === breadcrumbs.length - 1" class="max-w-64 truncate" :title="item.name">{{ item.name }}</BreadcrumbPage>
              <BreadcrumbLink v-else as-child>
                <button type="button" class="max-w-48 truncate" :disabled="deleting" :title="item.name" @click="loadDirectory(item.path)">{{ item.name }}</button>
              </BreadcrumbLink>
            </BreadcrumbItem>
          </template>
        </BreadcrumbList>
      </Breadcrumb>

      <Alert v-if="error" variant="destructive">
        <AlertCircle class="size-4" />
        <AlertTitle>无法加载目录</AlertTitle>
        <AlertDescription>{{ error }}，请刷新重试或返回父目录。</AlertDescription>
      </Alert>

      <div class="overflow-hidden rounded-md border" :aria-busy="loading">
        <Table class="table-fixed">
          <TableHeader>
            <TableRow>
              <TableHead class="w-12 pl-4">
                <Checkbox :model-value="selectionState" :disabled="loading || deleting || !visibleEntries.length" aria-label="选择当前显示的所有文件和文件夹" @update:model-value="selectVisible" />
              </TableHead>
              <TableHead class="text-center">名称</TableHead>
              <TableHead class="w-24 text-center">类型</TableHead>
              <TableHead class="w-28 text-center">大小</TableHead>
              <TableHead class="w-48 text-center">修改时间</TableHead>
              <TableHead class="w-44 pr-4 text-center">操作</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <template v-if="loading">
              <TableRow v-for="row in 5" :key="row">
                <TableCell class="pl-4"><Skeleton class="size-4" /></TableCell>
                <TableCell><Skeleton class="h-5 w-2/3" /></TableCell>
                <TableCell><Skeleton class="h-5 w-12" /></TableCell>
                <TableCell><Skeleton class="h-5 w-16" /></TableCell>
                <TableCell><Skeleton class="h-5 w-36" /></TableCell>
                <TableCell><Skeleton class="ml-auto h-8 w-32" /></TableCell>
              </TableRow>
            </template>
            <template v-else>
              <TableRow v-for="entry in visibleEntries" :key="entry.path" :data-state="selected.has(entry.path) ? 'selected' : undefined">
                <TableCell class="pl-4">
                  <Checkbox :model-value="selected.has(entry.path)" :disabled="deleting" :aria-label="`选择 ${entry.name}`" @update:model-value="checked => selectEntry(entry.path, checked)" />
                </TableCell>
                <TableCell>
                  <div class="flex items-center gap-2">
                    <button v-if="entry.type === 'directory'" type="button" class="flex min-w-0 items-center gap-3 text-left hover:underline" :disabled="deleting" :title="entry.name" @click="loadDirectory(entry.path)">
                      <Folder class="size-4 shrink-0 text-muted-foreground" />
                      <span class="truncate">{{ entry.name }}</span>
                    </button>
                    <a v-else-if="canPreview(entry)" :href="fileUrl('preview', entry.path)" target="_blank" rel="noopener noreferrer" class="flex min-w-0 items-center gap-3 hover:underline" :title="entry.name">
                      <File class="size-4 shrink-0 text-muted-foreground" />
                      <span class="truncate">{{ entry.name }}</span>
                    </a>
                    <div v-else class="flex min-w-0 items-center gap-3" :title="entry.name">
                      <File class="size-4 shrink-0 text-muted-foreground" />
                      <span class="truncate">{{ entry.name }}</span>
                    </div>
                    <Badge v-if="downloaded.has(entry.path)" variant="secondary" title="已点击下载，仅记录本次页面中的下载操作"><Check />已下载</Badge>
                  </div>
                </TableCell>
                <TableCell class="text-center text-muted-foreground">{{ entry.type === 'directory' ? '文件夹' : '文件' }}</TableCell>
                <TableCell class="text-right text-muted-foreground tabular-nums">{{ formatSize(entry.size) }}</TableCell>
                <TableCell class="text-center text-muted-foreground tabular-nums">{{ new Date(entry.modifiedAt).toLocaleString('zh-CN', { hour12: false }) }}</TableCell>
                <TableCell class="pr-4">
                  <div class="flex justify-center gap-1">
                    <Button v-if="entry.type === 'file'" variant="ghost" size="sm" as-child>
                      <a :href="fileUrl('download', entry.path)" :download="entry.name" @click="markDownloaded(entry.path)"><Download />下载</a>
                    </Button>
                  </div>
                </TableCell>
              </TableRow>
              <TableRow v-if="!visibleEntries.length">
                <TableCell :colspan="6" class="h-40 text-center text-muted-foreground">
                  {{ error ? '目录加载失败' : filter.trim() ? '没有匹配的文件或文件夹' : '此目录为空' }}
                </TableCell>
              </TableRow>
            </template>
          </TableBody>
        </Table>
      </div>
      <p v-if="!loading && !error" class="text-sm text-muted-foreground">
        {{ filter.trim() ? `${visibleEntries.length} 个匹配项，共 ${entries.length} 项` : `共 ${entries.length} 项` }}
      </p>
    </main>

    <form ref="archiveForm" action="/api/files/archive" method="post" target="mint-archive-download" class="hidden">
      <input type="hidden" name="paths" :value="JSON.stringify([...selected])">
    </form>
    <iframe name="mint-archive-download" class="hidden" title="打包下载" @load="archiveLoaded" />

    <AlertDialog :open="confirmingDelete" @update:open="open => { if (!deleting) confirmingDelete = open }">
      <AlertDialogContent @escape-key-down="deleting && $event.preventDefault()">
        <AlertDialogHeader>
          <AlertDialogTitle>删除所选项目？</AlertDialogTitle>
          <AlertDialogDescription>
            将永久删除 {{ selectedEntries.length - selectedDirectories }} 个文件和 {{ selectedDirectories }} 个文件夹，包含所选文件夹的全部内容。此操作无法撤销。
          </AlertDialogDescription>
        </AlertDialogHeader>
        <ul class="max-h-48 space-y-1 overflow-auto text-sm">
          <li v-for="entry in selectedEntries" :key="entry.path" class="truncate" :title="entry.name">{{ entry.name }}</li>
        </ul>
        <AlertDialogFooter>
          <AlertDialogCancel :disabled="deleting">取消</AlertDialogCancel>
          <Button variant="destructive" :disabled="deleting" @click="removeSelected">
            <LoaderCircle v-if="deleting" class="animate-spin" />
            {{ deleting ? '正在删除…' : '确认删除' }}
          </Button>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  </div>
</template>
