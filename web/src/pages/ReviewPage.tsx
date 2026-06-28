import { useEffect, useState } from 'react'
import type { Card, ClozeSpan, Rating } from '../types'
import { getCardsDue, postReview } from '../api'

function renderClozeHidden(text: string, spans: ClozeSpan[]): string {
  if (spans.length === 0) return text
  let result = ''
  let pos = 0
  for (const span of spans) {
    result += text.slice(pos, span.start) + '[...]'
    pos = span.end
  }
  return result + text.slice(pos)
}

export default function ReviewPage() {
  const [cards, setCards] = useState<Card[]>([])
  const [index, setIndex] = useState(0)
  const [revealed, setRevealed] = useState(false)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [submitError, setSubmitError] = useState<string | null>(null)

  useEffect(() => {
    const controller = new AbortController()
    getCardsDue(controller.signal)
      .then(cards => { setCards(cards); setLoading(false) })
      .catch(err => {
        if (err.name === 'AbortError') return
        setError('Failed to load cards')
        setLoading(false)
      })
    return () => controller.abort()
  }, [])

  async function rate(rating: Rating) {
    const card = cards[index]
    setSubmitError(null)
    setRevealed(false)
    setIndex(i => i + 1)
    try {
      await postReview(card.id, rating)
    } catch {
      setSubmitError('Failed to save review — please reload and try again')
    }
  }

  if (loading) return <p>Loading...</p>
  if (error) return <p>{error}</p>

  const card = cards[index]
  if (!card) return <p>{cards.length > 0 ? 'All done for today!' : 'No cards due. Come back later.'}</p>

  const { content } = card

  return (
    <div>
      {submitError && <p style={{ color: 'red' }}>{submitError}</p>}
      <p style={{ color: '#888', fontSize: '0.85em' }}>
        {index + 1} / {cards.length} — {card.source_file}
        {card.source_heading ? ` › ${card.source_heading}` : ''}
      </p>

      {content.type === 'qa' ? (
        <>
          <p><strong>Q:</strong> {content.front}</p>
          {revealed
            ? <p><strong>A:</strong> {content.back}</p>
            : <button onClick={() => setRevealed(true)}>Show answer</button>
          }
        </>
      ) : (
        <>
          <p>{revealed ? content.text : renderClozeHidden(content.text, content.spans)}</p>
          {!revealed && <button onClick={() => setRevealed(true)}>Show answer</button>}
        </>
      )}

      {revealed && (
        <div>
          <button onClick={() => rate('again')}>Again</button>
          <button onClick={() => rate('hard')}>Hard</button>
          <button onClick={() => rate('good')}>Good</button>
          <button onClick={() => rate('easy')}>Easy</button>
        </div>
      )}
    </div>
  )
}
