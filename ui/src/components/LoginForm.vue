<script setup lang="ts">
import type { LoginRequest, SessionInfo } from '@/types'
import { ref } from 'vue'
import { AlertCircle, FolderOpen, LoaderCircle } from 'lucide-vue-next'
import { api } from '@/api'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardFooter, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'

const emit = defineEmits<{ success: [session: SessionInfo] }>()
const username = ref('')
const password = ref('')
const submitting = ref(false)
const error = ref('')

async function login() {
  if (submitting.value) return
  submitting.value = true
  error.value = ''
  try {
    const body: LoginRequest = { username: username.value, password: password.value }
    const session = await api<SessionInfo>('/auth/login', { method: 'POST', body: JSON.stringify(body) })
    password.value = ''
    emit('success', session)
  } catch (cause) {
    error.value = (cause as Error).message
  } finally {
    submitting.value = false
  }
}
</script>

<template>
  <main class="flex min-h-screen items-center justify-center p-6">
    <Card class="w-full max-w-sm">
      <form @submit.prevent="login">
        <CardHeader class="mb-6 py-3 gap-3 flex items-center">
          <FolderOpen class="size-7" />
          <CardTitle class="text-2xl">Mint</CardTitle>
        </CardHeader>
        <CardContent class="space-y-5">
          <div class="space-y-2">
            <Label for="username" class="text-md">账号</Label>
            <Input id="username" v-model="username" name="username" autocomplete="username" required autofocus :disabled="submitting" />
          </div>
          <div class="space-y-2">
            <Label for="password" class="text-md">密码</Label>
            <Input id="password" v-model="password" name="password" type="password" autocomplete="current-password" required :disabled="submitting" />
          </div>
          <Alert v-if="error" variant="destructive">
            <AlertCircle class="size-4" />
            <AlertDescription>{{ error }}</AlertDescription>
          </Alert>
        </CardContent>
        <CardFooter class="mt-6">
          <Button type="submit" class="w-full" :disabled="submitting">
            <LoaderCircle v-if="submitting" class="animate-spin" />
            {{ submitting ? '正在登录…' : '登录' }}
          </Button>
        </CardFooter>
      </form>
    </Card>
  </main>
</template>
