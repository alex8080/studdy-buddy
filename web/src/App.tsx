import { BrowserRouter, Routes, Route, Navigate } from 'react-router-dom'
import ReviewPage from './pages/ReviewPage'

export default function App() {
  return (
    <BrowserRouter>
      <Routes>
        <Route path="/review" element={<ReviewPage />} />
        <Route path="*" element={<Navigate to="/review" replace />} />
      </Routes>
    </BrowserRouter>
  )
}
