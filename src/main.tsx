import { message } from 'antd'
import React from 'react'
import ReactDOM from 'react-dom/client'
import App from './App'
import GithubCorner from './components/GithubCorner'

// 全局提示超时兜底：单条 5s 自动消失，最多同时 3 条（避免堆叠成"常驻横幅"）
message.config({ duration: 5, maxCount: 3 })

const root = document.getElementById('app')

if (!root) {
  throw new Error('Root element not found')
}

ReactDOM.createRoot(root).render(
  <React.StrictMode>
    <GithubCorner />
    <App />
  </React.StrictMode>,
)
