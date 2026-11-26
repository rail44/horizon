//! The wiki view's sheet.

horizon_ui::css_module! {"
.wiki { position: relative; display: flex; flex-direction: column; height: 100%;
        min-width: 240px;
        outline: none; background: var(--background); color: var(--text);
        font-family: var(--font-family); font-size: var(--body-size);
        line-height: var(--body-line);
    .header { flex: none; display: flex; flex-direction: column; gap: var(--u8);
              padding: var(--u24) var(--u16) 0 var(--u16); }
    .bar { display: flex; align-items: center; gap: var(--u8); }
    .bar .space { flex: 1; }
    .back, .layout, .close { flex: none; padding: 0 var(--u8); border-radius: var(--radius);
                             font-size: var(--meta-size); line-height: calc(var(--u4) * 5);
                             color: var(--text-muted); cursor: pointer; }
    .back:hover, .layout:hover, .close:hover { background: var(--surface-raised); }
    .scrim { position: absolute; left: 0; top: 0; right: 0; bottom: 0; z-index: 10;
             display: flex; justify-content: center; align-items: flex-start;
             padding-top: calc(var(--u24) * 2); }
    .dialog { display: flex; flex-direction: column; gap: var(--u8);
              width: calc(var(--u24) * 30); max-width: 92%; max-height: 85%;
              box-sizing: border-box; padding: var(--u16);
              background: var(--background); border: var(--line) solid var(--border);
              border-radius: var(--radius-large);
              box-shadow: var(--shadow-modal); }
    .dialog .top { flex: none; display: flex; align-items: flex-start; gap: var(--u8); }
    .dialog .passage { flex: 1; min-width: 0; font-size: var(--meta-size);
                       line-height: var(--meta-line); color: var(--text-muted); }
    .dialog .posts { flex: 1; min-height: 0; overflow-y: scroll; display: flex;
                     flex-direction: column; gap: var(--u8); }
    .heading { font-size: var(--t1-size); line-height: var(--t1-line); font-weight: 600; }
    .fields { display: flex; flex-wrap: wrap; gap: var(--u4); }
    .state { flex: none; padding: var(--u24) var(--u16) 0 var(--u16);
             font-size: var(--meta-size); line-height: var(--meta-line);
             color: var(--text-muted); }
    .state.error { color: var(--danger); }
    .notice { flex: none; padding: var(--u8) var(--u16) 0 var(--u16); }
    .scroll { flex: 1; min-height: 0; overflow-y: scroll; display: flex;
              flex-direction: column; gap: var(--u8);
              padding: var(--u24) var(--u16) var(--u32) var(--u16); }
    .unit { position: absolute; width: var(--u16); height: 0; visibility: hidden; }
    .row { flex: none; display: flex; align-items: flex-start; gap: var(--u24); }
    .row.narrow { gap: 0; }
    .text { flex: 1; min-width: 0; display: flex; flex-direction: column;
            gap: var(--u8); }
    .margin { flex: none; position: relative; }
    .aside { position: absolute; left: 0; right: 0; }
    .thread, .new { display: flex; flex-direction: column; gap: var(--u4);
                    padding: var(--u4); border: var(--line) solid var(--border);
                    border-radius: var(--radius); }
    .thread.focused, .new { border-color: var(--accent); }
    .head, .quoted { display: flex; align-items: center; gap: var(--u2);
                     font-size: var(--meta-size); line-height: var(--meta-line);
                     color: var(--text-muted); }
    .head { cursor: pointer; }
    .quote { flex: 1; min-width: 0; overflow: hidden; white-space: nowrap;
             text-overflow: ellipsis; }
    .count { flex: none; }
    .post { display: flex; flex-direction: column; gap: var(--u2);
            padding: var(--u4) var(--u16); border: var(--line) solid var(--border);
            border-radius: var(--radius); }
    .post.person { margin-left: var(--u24); border: none; border-radius: 0;
                   border-left: var(--line-strong) solid var(--text-muted); }
    .when, .hint { font-size: var(--micro-size); line-height: var(--micro-line);
                   color: var(--text-muted); }
    .replying { position: relative; padding: var(--u4) var(--u16);
                border: var(--line) solid var(--border); border-radius: var(--radius);
                font-size: var(--meta-size); line-height: var(--meta-line);
                color: var(--text-muted); }
    .replying .bar { position: absolute; left: 0; top: 0; bottom: 0; width: var(--line-strong);
                     background: var(--accent); }
    .composer { display: flex; flex-direction: column; gap: var(--u2); padding-top: var(--u4); }
    .composer .field { padding: var(--u2) var(--u16); border: var(--line) solid var(--border);
                       border-radius: var(--radius); }
    .composer textarea { height: calc(var(--body-line) * 2); }
    .send { display: flex; align-items: center; justify-content: flex-end; gap: var(--u2); }
    .key { padding: 0 var(--u4); border: var(--line) solid var(--border); border-radius: 3px;
           font-size: var(--micro-size); line-height: var(--micro-line);
           color: var(--text-muted); }
    .offer { display: flex; align-items: center; gap: var(--u2); }
    .orphans { padding-top: var(--u24); font-size: var(--meta-size);
               line-height: var(--meta-line); font-weight: 600;
               color: var(--text-muted); }
}
"}
