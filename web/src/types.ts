export type CardId = string

export type CardContent =
  | { type: 'qa'; front: string; back: string }
  | { type: 'cloze'; text: string; spans: ClozeSpan[] }

export interface ClozeSpan {
  start: number
  end: number
  hint: string | null
}

export interface Card {
  id: CardId
  content: CardContent
  source_file: string
  source_heading: string | null
  tags: string[]
  status: CardStatus
  created_at: string
}

export type CardStatus = 'pending' | 'accepted' | 'rejected'

export type Rating = 'again' | 'hard' | 'good' | 'easy'
