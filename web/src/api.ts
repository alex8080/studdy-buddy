import type { Card, Rating } from './types'

const token = import.meta.env.PUBLIC_STUDYBUDDY_API_TOKEN as string | undefined

function authHeaders(): Record<string, string> {
  return token ? { Authorization: `Bearer ${token}` } : {}
}

export async function getCardsDue(signal?: AbortSignal): Promise<Card[]> {
  const res = await fetch('/cards/due', { headers: authHeaders(), signal })
  if (!res.ok) throw new Error(`GET /cards/due failed: ${res.status}`)
  const data = await res.json()
  return data.cards ?? []
}

export async function postReview(card_id: string, rating: Rating): Promise<void> {
  const res = await fetch('/reviews', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', ...authHeaders() },
    body: JSON.stringify({ card_id, rating }),
  })
  if (!res.ok) throw new Error(`POST /reviews failed: ${res.status}`)
}
