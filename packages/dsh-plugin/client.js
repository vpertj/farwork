// The browser half of @farwork/dsh-bridge: one sidebar entry plus a status panel.
//
// Hand-written CJS factory, on purpose. DSH's module loader evaluates this file as
// a script and calls load({ id, factory }), so a bundler, JSX, or any dependency
// beyond the modules DSH already ships would only add ways for the panel to break.
// Styles are inline: whether the bundled UI primitives' CSS Modules arrive with a
// bare require() is unverified, and a half-styled panel is worse than a plain one.
//
// Nothing in here may throw into DSH: apply() is wrapped whole, the panel renders
// its own failures, and a dead connection service costs the panel, not the session.
;(function () {
  var loader = typeof window === 'undefined' ? null : window.__ModuleLoader__
  if (!loader || typeof loader.load !== 'function') return

  loader.load({
    id: '@farwork/dsh-bridge',
    factory: function (require) {
      var React = require('react')
      var h = React.createElement

      // Not /api: the Typert gateway owns that channel and there is only one
      // interceptor slot per channel.
      var CHANNEL = '/farwork'
      var POLL_MS = 5000
      var LABEL = 'Farwork'
      var BOUNDARY = '审批/问答不在本插件 v1 内：需要审批的会话请在 DSH 自己的界面里回答。'

      var COLOR = {
        text: 'CanvasText',
        card: 'Canvas',
        border: 'rgba(128, 128, 128, 0.38)',
        fill: 'rgba(128, 128, 128, 0.14)',
        ok: '#1f9d55',
        warn: '#d97706',
        mono: 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
      }

      var overlayStyle = {
        position: 'fixed',
        inset: '0',
        zIndex: '2147483000',
        background: 'rgba(0, 0, 0, 0.28)',
      }

      var cardStyle = {
        position: 'fixed',
        right: '16px',
        bottom: '16px',
        zIndex: '2147483001',
        width: 'min(430px, calc(100vw - 32px))',
        maxHeight: 'min(78vh, 640px)',
        overflowY: 'auto',
        display: 'flex',
        flexDirection: 'column',
        gap: '10px',
        padding: '14px',
        borderRadius: '10px',
        background: COLOR.card,
        color: COLOR.text,
        border: '1px solid ' + COLOR.border,
        boxShadow: '0 12px 40px rgba(0, 0, 0, 0.32)',
        font: '13px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif',
        textAlign: 'left',
      }

      var muted = { fontSize: '11px', opacity: '0.72' }
      var blockStyle = {
        margin: '0',
        padding: '8px',
        borderRadius: '6px',
        background: COLOR.fill,
        font: '11px/1.5 ' + COLOR.mono,
        whiteSpace: 'pre-wrap',
        wordBreak: 'break-all',
        userSelect: 'text',
      }
      var buttonStyle = {
        padding: '4px 10px',
        borderRadius: '6px',
        border: '1px solid ' + COLOR.border,
        background: 'transparent',
        color: 'inherit',
        font: 'inherit',
        fontSize: '11px',
        cursor: 'pointer',
        whiteSpace: 'nowrap',
      }
      var inputStyle = {
        flex: '1 1 auto',
        minWidth: '0',
        padding: '4px 8px',
        borderRadius: '6px',
        border: '1px solid ' + COLOR.border,
        background: COLOR.card,
        color: 'inherit',
        font: '11px/1.5 ' + COLOR.mono,
      }

      function warn(message, error) {
        try {
          console.warn('[farwork] ' + message, error)
        } catch (ignored) {
          /* a logger that throws is worse than a lost line */
        }
      }

      function describeError(error) {
        if (!error) return 'unknown error'
        if (typeof error === 'string') return error
        if (typeof error.message === 'string' && error.message.length > 0) return error.message
        return 'unknown error'
      }

      function resolveSlots(ctx) {
        if (!ctx) return null
        try {
          if (ctx.slots) return ctx.slots
        } catch (ignored) {
          /* a guarded getter may still throw */
        }
        try {
          if (typeof ctx.get === 'function') return ctx.get('slots') || null
        } catch (ignored) {
          return null
        }
        return null
      }

      function resolveConnection(ctx) {
        if (!ctx) return null
        try {
          var direct = ctx.connection
          if (direct && typeof direct.rpc === 'object' && direct.rpc !== null) return direct
        } catch (ignored) {
          /* fall through to ctx.get */
        }
        try {
          if (typeof ctx.get === 'function') {
            var viaGet = ctx.get('connection')
            if (viaGet) return viaGet
          }
        } catch (ignored) {
          return null
        }
        return null
      }

      function rpcCall(ctx, endpoint, payload) {
        return new Promise(function (resolve, reject) {
          var connection = resolveConnection(ctx)
          var rpc = connection && connection.rpc
          if (!rpc || typeof rpc.call !== 'function') {
            reject(new Error('connection service unavailable'))
            return
          }
          var pending
          try {
            pending = rpc.call(CHANNEL, endpoint, payload || {})
          } catch (error) {
            reject(error)
            return
          }
          Promise.resolve(pending).then(function (reply) {
            // The host always answers with an envelope; a refusal is a rendered
            // line, never a silent empty panel.
            if (!reply || reply.ok !== true) {
              var failure = reply && reply.error
              reject(new Error(failure && failure.message ? String(failure.message) : 'request failed'))
              return
            }
            resolve(reply.value || {})
          }, reject)
        })
      }

      function describeFailure(error) {
        var message = describeError(error)
        if (/unauthor|forbidden|not signed in/i.test(message)) return '未授权，请刷新页面后重试'
        // Nothing is registered on /farwork: the browser reached the host, so
        // blaming "the connection service" would send the user to the wrong place.
        if (/channel|not registered|no handler|unregistered/i.test(message)) {
          return '面板通道未注册（host 侧没能登记 /farwork），请查看 DSH 日志里 farwork-dsh-bridge 的行'
        }
        // 404/405 on the channel path means nothing is mounted for /farwork at all:
        // the request reached DSH's web server and no host handler claimed it.
        if (/\b40[45]\b/.test(message)) {
          return '面板通道未注册（host 侧没能登记 /farwork），请查看 DSH 日志里 farwork-dsh-bridge 的行'
        }
        if (/unavailable/i.test(message)) return '连接服务不可用，请刷新页面'
        // Always a sentence: an empty string here would render an empty panel.
        return message.length > 0 ? message : '连接服务不可用，请刷新页面'
      }

      function copyText(value) {
        return new Promise(function (resolve, reject) {
          try {
            if (typeof navigator !== 'undefined' && navigator.clipboard && typeof navigator.clipboard.writeText === 'function') {
              navigator.clipboard.writeText(value).then(resolve, reject)
              return
            }
            reject(new Error('clipboard unavailable'))
          } catch (error) {
            reject(error)
          }
        })
      }

      var portalImpl
      function getCreatePortal() {
        if (portalImpl !== undefined) return portalImpl
        portalImpl = null
        try {
          var dom = require('react-dom')
          if (dom && typeof dom.createPortal === 'function') portalImpl = dom.createPortal
        } catch (ignored) {
          portalImpl = null
        }
        return portalImpl
      }

      function withPortal(node) {
        var create = getCreatePortal()
        if (!create || typeof document === 'undefined' || !document.body) return node
        try {
          return create(node, document.body)
        } catch (ignored) {
          return node
        }
      }

      function icon(size) {
        return h(
          'svg',
          { width: size, height: size, viewBox: '0 0 16 16', 'aria-hidden': 'true', style: { flex: '0 0 auto' } },
          h('circle', { cx: '8', cy: '8', r: '6.2', fill: 'none', stroke: 'currentColor', strokeWidth: '1.4' }),
          h('path', {
            d: 'M8 4.6v3.6l2.6 1.7',
            fill: 'none',
            stroke: 'currentColor',
            strokeWidth: '1.4',
            strokeLinecap: 'round',
          }),
        )
      }

      function dot(color) {
        return { width: '8px', height: '8px', borderRadius: '50%', background: color, flex: '0 0 auto' }
      }

      function Panel(props) {
        var ctx = props.ctx
        var onClose = props.onClose
        var statusPair = React.useState(null)
        var status = statusPair[0]
        var setStatus = statusPair[1]
        var errorPair = React.useState('')
        var error = errorPair[0]
        var setError = errorPair[1]
        var commandPair = React.useState('')
        var command = commandPair[0]
        var setCommand = commandPair[1]
        var tokenPair = React.useState('')
        var token = tokenPair[0]
        var setToken = tokenPair[1]
        var noticePair = React.useState('')
        var notice = noticePair[0]
        var setNotice = noticePair[1]

        React.useEffect(function () {
          var alive = true
          function refresh() {
            rpcCall(ctx, 'status', {}).then(
              function (value) {
                if (!alive) return
                setStatus(value)
                setError('')
              },
              function (failure) {
                if (!alive) return
                setError(describeFailure(failure))
              },
            )
            rpcCall(ctx, 'command', {}).then(
              function (value) {
                if (!alive) return
                setCommand(value && typeof value.command === 'string' ? value.command : '')
              },
              function (ignored) {
                /* the status line already carries the connection error */
              },
            )
          }
          refresh()
          // Polling, not websockets: the bridge state changes at start/stop time,
          // and one loopback round trip per 5s is cheaper than another subscription.
          var timer = setInterval(refresh, POLL_MS)
          return function () {
            alive = false
            clearInterval(timer)
          }
        }, [])

        function copy(value, what) {
          copyText(value).then(
            function () {
              setNotice(what + '已复制')
            },
            function () {
              setNotice(what + '复制失败，请手动选择')
            },
          )
        }

        function regenerate() {
          var typed = token.trim()
          var payload = typed.length > 0 ? { connectorToken: typed } : {}
          setNotice('正在更新命令…')
          // savePair first: the host is the one place a pasted token is allowed to
          // persist, and a command built from it must survive a panel close.
          rpcCall(ctx, 'savePair', payload)
            .then(function () {
              return rpcCall(ctx, 'command', payload)
            })
            .then(
              function (value) {
                setCommand(value && typeof value.command === 'string' ? value.command : '')
                setNotice(typed.length > 0 ? '已保存 token 并更新命令' : '命令已更新')
              },
              function (failure) {
                setNotice(describeFailure(failure))
              },
            )
        }

        var ready = status !== null && status.ready === true
        var badgeColor = status === null ? COLOR.border : ready ? COLOR.ok : COLOR.warn
        var badgeText = status === null ? '读取中…' : ready ? '就绪' : '未就绪'
        var endpointPath = status && typeof status.endpointPath === 'string' ? status.endpointPath : ''
        var port = status && typeof status.port === 'number' ? status.port : 0
        var lastError = status && typeof status.lastError === 'string' ? status.lastError : ''

        return h(
          React.Fragment,
          null,
          h('div', { style: overlayStyle, onClick: onClose }),
          h(
            'div',
            { role: 'dialog', 'aria-label': 'Farwork', style: cardStyle },
            h(
              'div',
              { style: { display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: '8px' } },
              h(
                'div',
                { style: { fontWeight: '600', fontSize: '13px' } },
                LABEL,
                status && typeof status.version === 'string' ? h('span', { style: muted }, ' ' + status.version) : null,
              ),
              h('button', { type: 'button', onClick: onClose, 'aria-label': '关闭', style: buttonStyle }, '关闭'),
            ),
            h(
              'div',
              { style: { display: 'flex', alignItems: 'center', gap: '8px' } },
              h('span', { style: dot(badgeColor) }),
              h('span', { style: { fontSize: '12px' } }, badgeText),
              port > 0 ? h('span', { style: muted }, '端口 ' + String(port)) : null,
              status && status.pid ? h('span', { style: muted }, 'pid ' + String(status.pid)) : null,
            ),
            lastError.length > 0
              ? h('div', null, h('div', { style: muted }, '失败原因'), h('pre', { style: blockStyle }, lastError))
              : null,
            error.length > 0 ? h('div', { style: { fontSize: '12px', color: COLOR.warn } }, error) : null,
            h(
              'div',
              null,
              h('div', { style: muted }, 'endpoint（agent 从这里发现本桥）'),
              h(
                'div',
                { style: { display: 'flex', alignItems: 'center', gap: '8px' } },
                h('pre', { style: Object.assign({}, blockStyle, { flex: '1 1 auto' }) }, endpointPath || '（尚未确定）'),
                h(
                  'button',
                  {
                    type: 'button',
                    style: buttonStyle,
                    onClick: function () {
                      copy(endpointPath, 'endpoint 路径')
                    },
                  },
                  '复制',
                ),
              ),
            ),
            h(
              'div',
              null,
              h('div', { style: muted }, '在本机运行 farwork-agent'),
              h('pre', { style: blockStyle }, command || '（读取中…）'),
              h(
                'div',
                { style: { display: 'flex', gap: '8px', marginTop: '6px' } },
                h(
                  'button',
                  {
                    type: 'button',
                    style: buttonStyle,
                    onClick: function () {
                      copy(command, '命令')
                    },
                  },
                  '复制命令',
                ),
              ),
            ),
            h(
              'div',
              null,
              h('div', { style: muted }, '粘贴 hub 的 connectorToken（只用于拼命令，不回传浏览器）'),
              h(
                'div',
                { style: { display: 'flex', gap: '8px', marginTop: '4px' } },
                h('input', {
                  type: 'password',
                  style: inputStyle,
                  value: token,
                  placeholder: 'connectorToken',
                  autoComplete: 'off',
                  spellCheck: false,
                  'aria-label': 'connectorToken',
                  onChange: function (event) {
                    setToken(event && event.target ? String(event.target.value) : '')
                  },
                }),
                h('button', { type: 'button', style: buttonStyle, onClick: regenerate }, '重新生成命令'),
              ),
            ),
            notice.length > 0 ? h('div', { style: muted }, notice) : null,
            h('div', { style: { fontSize: '11px', opacity: '0.8', borderTop: '1px solid ' + COLOR.border, paddingTop: '8px' } }, BOUNDARY),
          ),
        )
      }

      function entryStyle(wide) {
        return {
          display: 'flex',
          alignItems: 'center',
          justifyContent: wide ? 'flex-start' : 'center',
          gap: '8px',
          width: '100%',
          padding: '6px 10px',
          margin: '2px 0',
          border: '1px solid transparent',
          borderRadius: '6px',
          background: 'transparent',
          color: 'inherit',
          font: 'inherit',
          fontSize: '12px',
          lineHeight: '18px',
          textAlign: 'left',
          cursor: 'pointer',
          overflow: 'hidden',
        }
      }

      function makeEntry(ctx) {
        return function FarworkEntry(props) {
          // A collapsed sidebar gives the slot next to no width; the label would be
          // clipped mid-word, so it shows the mark alone and keeps the accessible name.
          var wide = !props || props.wide !== false
          var pair = React.useState(false)
          var open = pair[0]
          var setOpen = pair[1]
          var button = h(
            'button',
            {
              type: 'button',
              title: LABEL,
              'aria-label': LABEL,
              'aria-expanded': open ? 'true' : 'false',
              onClick: function () {
                setOpen(!open)
              },
              style: entryStyle(wide),
            },
            icon(16),
            wide
              ? h('span', { style: { overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' } }, LABEL)
              : null,
          )
          if (!open) return button
          return h(
            React.Fragment,
            null,
            button,
            withPortal(
              h(Panel, {
                ctx: ctx,
                onClose: function () {
                  setOpen(false)
                },
              }),
            ),
          )
        }
      }

      function apply(ctx) {
        try {
          var slots = resolveSlots(ctx)
          if (!slots || typeof slots.inject !== 'function' || typeof slots.register !== 'function') {
            warn('the slots service is unavailable; the Farwork entry was not registered')
            return
          }
          var Entry = makeEntry(ctx)
          slots.inject('sidebar.footer.action', function () {
            try {
              return slots.register(
                {
                  name: 'sidebar.footer.action',
                  id: 'farwork',
                  // 26 is the footer row just above the settings gear; the slot is a
                  // list, so order is the only thing that decides placement.
                  order: 26,
                  label: function () {
                    return LABEL
                  },
                },
                Entry,
              )
            } catch (error) {
              warn('the Farwork entry could not be registered', error)
              return function () {}
            }
          })
        } catch (error) {
          // A broken panel must never be the reason DSH fails to start.
          warn('the Farwork client could not be applied', error)
        }
      }

      return { inject: ['slots'], apply: apply }
    },
  })
})()
