<script setup lang="ts">
import type { SessionInfo } from '@/types'
import { onMounted, ref } from 'vue'
import { AlertCircle, FolderOpen } from 'lucide-vue-next'
import { toast } from 'vue-sonner'
import { api, ApiError } from '@/api'
import FileBrowser from '@/components/FileBrowser.vue'
import LoginForm from '@/components/LoginForm.vue'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { Toaster } from '@/components/ui/sonner'

const session = ref<SessionInfo | null>(null)
const loading = ref(true)
const error = ref('')
const loggingOut = ref(false)

async function loadSession() {
  loading.value = true
  error.value = ''
  try {
    session.value = await api<SessionInfo>('/auth/me')
  } catch (cause) {
    if (!(cause instanceof ApiError && cause.status === 401)) error.value = (cause as Error).message
  } finally {
    loading.value = false
  }
}

async function logout() {
  loggingOut.value = true
  try {
    await api<void>('/auth/logout', { method: 'POST' })
    session.value = null
  } catch (cause) {
    toast.error((cause as Error).message)
  } finally {
    loggingOut.value = false
  }
}

onMounted(loadSession)
</script>

<template>
  <div v-if="loading" class="flex min-h-screen items-center justify-center" aria-label="正在加载">
    <div class="flex w-80 flex-col gap-4">
      <FolderOpen class="mx-auto size-8 text-muted-foreground" />
      <Skeleton class="h-5 w-full" />
      <Skeleton class="h-5 w-2/3" />
    </div>
  </div>
  <div v-else-if="error" class="flex min-h-screen items-center justify-center p-6">
    <div class="w-full max-w-md space-y-4">
      <Alert variant="destructive">
        <AlertCircle class="size-4" />
        <AlertTitle>无法连接服务</AlertTitle>
        <AlertDescription>{{ error }}</AlertDescription>
      </Alert>
      <Button variant="outline" @click="loadSession">重试</Button>
    </div>
  </div>
  <FileBrowser v-else-if="session" :username="session.username" :logging-out="loggingOut" @logout="logout" @unauthorized="session = null" />
  <LoginForm v-else @success="session = $event" />
  <Toaster position="bottom-right" />
</template>
