export class ApiError extends Error {
  status: number

  constructor(message: string, status: number) {
    super(message)
    this.status = status
  }
}

export async function api<T>(path: string, options: RequestInit = {}): Promise<T> {
  const response = await fetch(`/api${path}`, {
    ...options,
    headers: options.body ? { 'Content-Type': 'application/json', ...options.headers } : options.headers,
  })

  if (!response.ok) {
    const body: { message: string } = await response.json()
    throw new ApiError(body.message, response.status)
  }

  if (response.status === 204) return undefined as T
  return response.json() as Promise<T>
}
