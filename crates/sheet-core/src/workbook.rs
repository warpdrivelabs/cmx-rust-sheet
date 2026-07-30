//! 工作簿：工作表集合 + 活动表 + 事件 + 命令/撤销管理 + 命名区域。
//!
//! 对标 cmx-megasheet 的 Workbook.ts。模型树顶层。
//!
//! Rust 移植取舍（对 TS 的 GC 闭包体系的忠实翻译）：
//!  - 事件：`EventEmitter` 用 `RefCell<Vec<Box<dyn FnMut>>>` 存回调，`emit(&self)` 无需 &mut。
//!    事件载荷带索引 + 表名（不带 `&Worksheet` 引用，规避借用环；测试只读索引）。
//!  - 撤销：`UndoManager` 存 `Box<dyn UndoableAction>`；闭包动作用 `Rc<RefCell<_>>` 捕获外部态
//!    （对齐 TS 里两个闭包共享 `state`——Rust 无 GC，用内部可变性等价表达）。
//!  - 命令：`CommandManager` 持共享 `Rc<RefCell<UndoManager>>`，可撤销命令包成动作入栈。
//!  - 因含 `dyn FnMut` 回调，Workbook 非 Send；rayon 批量导出在门面层对**独立只读工作簿**
//!    并行（不跨线程共享带 live handler 的实例），不冲突。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::worksheet::Worksheet;

// ── 事件 ─────────────────────────────────────────────────

/// 工作簿级事件类型（bind 的键）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkbookEventKind {
    ActiveSheetChanged,
    SheetAdded,
    SheetRemoved,
}

/// 工作簿级事件载荷。
#[derive(Debug, Clone)]
pub enum WorkbookEvent {
    ActiveSheetChanged {
        old_index: usize,
        new_index: usize,
        name: String,
    },
    SheetAdded {
        index: usize,
        name: String,
    },
    SheetRemoved {
        index: usize,
        name: String,
    },
}

type Handler = Box<dyn FnMut(&WorkbookEvent)>;

/// 极简类型化事件总线。emit 借用 RefCell 而非 &mut self（对齐 TS 语义）。
#[derive(Default)]
pub struct EventEmitter {
    handlers: RefCell<HashMap<WorkbookEventKind, Vec<Handler>>>,
}

impl EventEmitter {
    pub fn new() -> Self {
        EventEmitter {
            handlers: RefCell::new(HashMap::new()),
        }
    }

    pub fn bind(&self, event: WorkbookEventKind, handler: Handler) {
        self.handlers
            .borrow_mut()
            .entry(event)
            .or_default()
            .push(handler);
    }

    /// 退订某事件的全部回调（Rust 无法比较 FnMut 身份，故不支持退订单个）。
    pub fn unbind(&self, event: WorkbookEventKind) {
        self.handlers.borrow_mut().remove(&event);
    }

    pub fn unbind_all(&self) {
        self.handlers.borrow_mut().clear();
    }

    pub fn has_listeners(&self, event: WorkbookEventKind) -> bool {
        self.handlers
            .borrow()
            .get(&event)
            .is_some_and(|v| !v.is_empty())
    }

    fn emit(&self, kind: WorkbookEventKind, ev: &WorkbookEvent) {
        if let Some(list) = self.handlers.borrow_mut().get_mut(&kind) {
            for h in list.iter_mut() {
                h(ev);
            }
        }
    }
}

// ── 撤销 ─────────────────────────────────────────────────

/// 结构编辑元信息（插/删行列），附在命令上供消费方同步地址键映射。
#[derive(Debug, Clone)]
pub struct StructuralEditMeta {
    pub axis: Axis,
    pub op: StructuralOp,
    pub index: u32,
    pub count: u32,
    pub sheet: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Row,
    Col,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuralOp {
    Insert,
    Delete,
}

/// 可撤销动作。执行/撤销为对称操作。
pub trait UndoableAction {
    fn name(&self) -> &str;
    fn execute(&mut self);
    fn undo(&mut self);
    /// 结构编辑命令带此元信息；普通命令无。
    fn structural(&self) -> Option<&StructuralEditMeta> {
        None
    }
}

/// 闭包式动作：一对 do/undo 闭包 + 名称。闭包用 `Rc<RefCell<_>>` 捕获共享态。
pub struct ClosureAction {
    name: String,
    exec: Box<dyn FnMut()>,
    undo_fn: Box<dyn FnMut()>,
    meta: Option<StructuralEditMeta>,
}

impl ClosureAction {
    pub fn new(
        name: impl Into<String>,
        exec: impl FnMut() + 'static,
        undo_fn: impl FnMut() + 'static,
    ) -> Self {
        ClosureAction {
            name: name.into(),
            exec: Box::new(exec),
            undo_fn: Box::new(undo_fn),
            meta: None,
        }
    }

    pub fn with_meta(mut self, meta: StructuralEditMeta) -> Self {
        self.meta = Some(meta);
        self
    }
}

impl UndoableAction for ClosureAction {
    fn name(&self) -> &str {
        &self.name
    }
    fn execute(&mut self) {
        (self.exec)();
    }
    fn undo(&mut self) {
        (self.undo_fn)();
    }
    fn structural(&self) -> Option<&StructuralEditMeta> {
        self.meta.as_ref()
    }
}

/// 撤销管理器：一对做/撤动作栈。
pub struct UndoManager {
    undo_stack: Vec<Box<dyn UndoableAction>>,
    redo_stack: Vec<Box<dyn UndoableAction>>,
    max_size: usize,
}

impl Default for UndoManager {
    fn default() -> Self {
        UndoManager {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            max_size: 100,
        }
    }
}

impl UndoManager {
    pub fn new() -> Self {
        UndoManager::default()
    }

    pub fn max_size(&self) -> usize {
        self.max_size
    }

    pub fn set_max_size(&mut self, n: usize) {
        self.max_size = n;
        self.trim();
    }

    /// 执行并压栈（清空 redo）。
    pub fn do_action(&mut self, mut action: Box<dyn UndoableAction>) {
        action.execute();
        self.undo_stack.push(action);
        self.redo_stack.clear();
        self.trim();
    }

    /// 仅压栈（动作已在外部执行），清空 redo。
    pub fn push(&mut self, action: Box<dyn UndoableAction>) {
        self.undo_stack.push(action);
        self.redo_stack.clear();
        self.trim();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// 撤销栈顶；空栈返回 false。
    pub fn undo(&mut self) -> bool {
        if let Some(mut a) = self.undo_stack.pop() {
            a.undo();
            self.redo_stack.push(a);
            true
        } else {
            false
        }
    }

    /// 重做栈顶；空栈返回 false。
    pub fn redo(&mut self) -> bool {
        if let Some(mut a) = self.redo_stack.pop() {
            a.execute();
            self.undo_stack.push(a);
            true
        } else {
            false
        }
    }

    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
    }

    pub fn undo_stack_len(&self) -> usize {
        self.undo_stack.len()
    }

    pub fn redo_stack_len(&self) -> usize {
        self.redo_stack.len()
    }

    fn trim(&mut self) {
        while self.undo_stack.len() > self.max_size {
            self.undo_stack.remove(0);
        }
    }
}

// ── 命令 ─────────────────────────────────────────────────

/// 命令执行选项（对齐 TS `execute({cmd, ...})`）。args 承载 tag/自定义键。
#[derive(Debug, Clone, Default)]
pub struct CommandOptions {
    pub cmd: String,
    pub name: Option<String>,
    pub args: HashMap<String, String>,
}

impl CommandOptions {
    pub fn new(cmd: &str) -> Self {
        CommandOptions {
            cmd: cmd.to_string(),
            ..Default::default()
        }
    }
    pub fn arg(mut self, key: &str, val: &str) -> Self {
        self.args.insert(key.to_string(), val.to_string());
        self
    }
    pub fn named(mut self, name: &str) -> Self {
        self.name = Some(name.to_string());
        self
    }
}

/// 命令回调：接收执行选项。
pub type CommandFn = Box<dyn FnMut(&CommandOptions)>;

/// 命名命令：execute + 可选 undo，皆接收选项。
pub struct Command {
    pub can_undo: bool,
    pub execute: CommandFn,
    pub undo: Option<CommandFn>,
}

/// 命令管理器：注册命名命令 + 执行；可撤销命令经共享 UndoManager 记录。
pub struct CommandManager {
    commands: HashMap<String, Rc<RefCell<Command>>>,
    undo: Rc<RefCell<UndoManager>>,
}

impl CommandManager {
    fn new(undo: Rc<RefCell<UndoManager>>) -> Self {
        CommandManager {
            commands: HashMap::new(),
            undo,
        }
    }

    pub fn register(&mut self, name: &str, command: Command) {
        self.commands
            .insert(name.to_string(), Rc::new(RefCell::new(command)));
    }

    pub fn has(&self, name: &str) -> bool {
        self.commands.contains_key(name)
    }

    /// 执行命令。可撤销命令包成 UndoableAction 入栈。未知命令返回 false。
    pub fn execute(&mut self, options: CommandOptions) -> bool {
        let Some(cmd_rc) = self.commands.get(&options.cmd).cloned() else {
            return false;
        };
        // 先跑一次 execute
        {
            let mut cmd = cmd_rc.borrow_mut();
            (cmd.execute)(&options);
        }
        let can_undo = { cmd_rc.borrow().can_undo && cmd_rc.borrow().undo.is_some() };
        if can_undo {
            let name = options.name.clone().unwrap_or_else(|| options.cmd.clone());
            let exec_rc = cmd_rc.clone();
            let exec_opts = options.clone();
            let undo_rc = cmd_rc.clone();
            let undo_opts = options.clone();
            let action = ClosureAction::new(
                name,
                move || {
                    let mut c = exec_rc.borrow_mut();
                    (c.execute)(&exec_opts);
                },
                move || {
                    let mut c = undo_rc.borrow_mut();
                    if let Some(u) = c.undo.as_mut() {
                        u(&undo_opts);
                    }
                },
            );
            self.undo.borrow_mut().push(Box::new(action));
        }
        true
    }
}

// ── 工作簿 ───────────────────────────────────────────────

/// 视口态（M9/M19）：冻结窗格 + 尾冻结 + 拆分模式。中性——由 element 层注入/消费，
/// IO 只透传（不影响数据模型）。纯后端引擎存储它以保证快照无损往返。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ViewportState {
    /// 冻结的行数（顶部钉住）。
    pub frozen_row_count: u32,
    /// 冻结的列数（左侧钉住）。
    pub frozen_col_count: u32,
    /// M19 尾冻结：钉视口末端的行数。
    pub trailing_row_count: u32,
    pub trailing_col_count: u32,
    /// M19-step2：冻结线是否为可拖拆分条。
    pub split_row: bool,
    pub split_col: bool,
}

/// 单元格是否落在冻结带内（index < frozen_count）。纯判定，对齐 PaneLayout.isFrozen。
pub fn is_frozen(index: u32, frozen_count: u32) -> bool {
    index < frozen_count
}

pub struct Workbook {
    sheets: Vec<Worksheet>,
    active_index: usize,
    paint_suspend: u32,
    events: EventEmitter,
    undo: Rc<RefCell<UndoManager>>,
    command_mgr: CommandManager,
    /// 命名区域 / Defined Names（M8）。键 = "scope NAME"（大写）。
    defined_names: HashMap<String, DefinedName>,
    /// 视口态（M9 冻结/M19 尾冻结/拆分）。
    viewport: ViewportState,
}

#[derive(Debug, Clone)]
struct DefinedName {
    scope: String,
    refers_to: String,
}

impl Default for Workbook {
    fn default() -> Self {
        Workbook::with_sheets(1)
    }
}

impl Workbook {
    /// 新建含 n 张空表的工作簿（Sheet1..SheetN）。
    pub fn with_sheets(n: usize) -> Self {
        let undo = Rc::new(RefCell::new(UndoManager::new()));
        let mut sheets = Vec::with_capacity(n);
        for i in 0..n {
            sheets.push(Worksheet::new(&format!("Sheet{}", i + 1)));
        }
        Workbook {
            sheets,
            active_index: 0,
            paint_suspend: 0,
            events: EventEmitter::new(),
            command_mgr: CommandManager::new(undo.clone()),
            undo,
            defined_names: HashMap::new(),
            viewport: ViewportState::default(),
        }
    }

    /// 空工作簿（0 张表）。
    pub fn empty() -> Self {
        Workbook::with_sheets(0)
    }

    // ── 工作表集合 ───────────────────────────────────────
    pub fn sheet_count(&self) -> usize {
        self.sheets.len()
    }

    pub fn sheet(&self, index: usize) -> Option<&Worksheet> {
        self.sheets.get(index)
    }

    pub fn sheet_mut(&mut self, index: usize) -> Option<&mut Worksheet> {
        self.sheets.get_mut(index)
    }

    pub fn active_sheet(&self) -> Option<&Worksheet> {
        self.sheets.get(self.active_index)
    }

    pub fn active_sheet_mut(&mut self) -> Option<&mut Worksheet> {
        self.sheets.get_mut(self.active_index)
    }

    pub fn active_sheet_index(&self) -> usize {
        self.active_index
    }

    pub fn set_active_sheet_index(&mut self, index: usize) {
        let next = index.min(self.sheets.len().saturating_sub(1));
        if self.sheets.is_empty() || next == self.active_index {
            return;
        }
        let old = self.active_index;
        self.active_index = next;
        if let Some(s) = self.sheets.get(next) {
            self.events.emit(
                WorkbookEventKind::ActiveSheetChanged,
                &WorkbookEvent::ActiveSheetChanged {
                    old_index: old,
                    new_index: next,
                    name: s.name().to_string(),
                },
            );
        }
    }

    /// 在 index 处插入工作表（越界则追加）。返回插入位置。
    pub fn add_sheet(&mut self, index: usize, sheet: Worksheet) -> usize {
        let at = index.min(self.sheets.len());
        let name = sheet.name().to_string();
        self.sheets.insert(at, sheet);
        if self.active_index >= at {
            self.active_index = (self.active_index + 1).min(self.sheets.len() - 1);
        }
        self.events.emit(
            WorkbookEventKind::SheetAdded,
            &WorkbookEvent::SheetAdded { index: at, name },
        );
        at
    }

    /// 追加工作表，返回其索引。
    pub fn append_sheet(&mut self, sheet: Worksheet) -> usize {
        self.add_sheet(self.sheets.len(), sheet)
    }

    /// 追加一张自动命名的空表，返回其索引。
    pub fn append_new_sheet(&mut self) -> usize {
        let name = format!("Sheet{}", self.sheets.len() + 1);
        self.append_sheet(Worksheet::new(&name))
    }

    /// 移除 index 处工作表。
    pub fn remove_sheet(&mut self, index: usize) {
        if index >= self.sheets.len() {
            return;
        }
        let ws = self.sheets.remove(index);
        if self.active_index >= self.sheets.len() {
            self.active_index = self.sheets.len().saturating_sub(1);
        }
        self.events.emit(
            WorkbookEventKind::SheetRemoved,
            &WorkbookEvent::SheetRemoved {
                index,
                name: ws.name().to_string(),
            },
        );
    }

    /// 移动工作表：from→to（页签拖拽排序）。活动表按身份跟随。
    pub fn move_sheet(&mut self, from: usize, to: usize) {
        let n = self.sheets.len();
        if from >= n {
            return;
        }
        let dest = to.min(n - 1);
        if dest == from {
            return;
        }
        let active_was = self.active_index;
        let moved = self.sheets.remove(from);
        self.sheets.insert(dest, moved);
        // 活动索引跟随原活动表新位置
        self.active_index = reindex_after_move(active_was, from, dest);
    }

    pub fn clear_sheets(&mut self) {
        self.sheets.clear();
        self.active_index = 0;
    }

    pub fn sheets(&self) -> &[Worksheet] {
        &self.sheets
    }

    pub fn sheets_mut(&mut self) -> &mut [Worksheet] {
        &mut self.sheets
    }

    pub fn sheet_by_name(&self, name: &str) -> Option<&Worksheet> {
        self.sheets.iter().find(|s| s.name() == name)
    }

    pub fn sheet_by_name_mut(&mut self, name: &str) -> Option<&mut Worksheet> {
        self.sheets.iter_mut().find(|s| s.name() == name)
    }

    pub fn index_of_sheet(&self, name: &str) -> Option<usize> {
        self.sheets.iter().position(|s| s.name() == name)
    }

    // ── 事件 ─────────────────────────────────────────────
    pub fn bind(&self, event: WorkbookEventKind, handler: Handler) {
        self.events.bind(event, handler);
    }

    pub fn unbind(&self, event: WorkbookEventKind) {
        self.events.unbind(event);
    }

    pub fn events(&self) -> &EventEmitter {
        &self.events
    }

    // ── 命令 / 撤销 ─────────────────────────────────────
    pub fn command_manager(&mut self) -> &mut CommandManager {
        &mut self.command_mgr
    }

    /// 共享撤销管理器句柄（`.borrow_mut()` 直接 do/undo/redo）。
    pub fn undo_manager(&self) -> Rc<RefCell<UndoManager>> {
        self.undo.clone()
    }

    pub fn can_undo(&self) -> bool {
        self.undo.borrow().can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.undo.borrow().can_redo()
    }

    pub fn undo(&self) -> bool {
        self.undo.borrow_mut().undo()
    }

    pub fn redo(&self) -> bool {
        self.undo.borrow_mut().redo()
    }

    // ── 命名区域（M8）─────────────────────────────────────
    pub fn define_name(&mut self, name: &str, refers_to: &str, scope: &str) {
        self.defined_names.insert(
            name_key(name, scope),
            DefinedName {
                scope: scope.to_string(),
                refers_to: refers_to.trim_start_matches('=').to_string(),
            },
        );
    }

    pub fn delete_name(&mut self, name: &str, scope: &str) -> bool {
        self.defined_names.remove(&name_key(name, scope)).is_some()
    }

    /// 解析命名区域 refersTo：先查 sheet 级，再查工作簿级。
    pub fn resolve_name(&self, name: &str, sheet_name: Option<&str>) -> Option<String> {
        let upper = name.to_uppercase();
        if let Some(sheet) = sheet_name {
            if let Some(local) = self.defined_names.get(&name_key(&upper, sheet)) {
                return Some(local.refers_to.clone());
            }
        }
        self.defined_names
            .get(&name_key(&upper, "workbook"))
            .map(|d| d.refers_to.clone())
    }

    pub fn list_names(&self) -> Vec<(String, String, String)> {
        self.defined_names
            .iter()
            .map(|(key, v)| {
                let name = key[key.find(' ').map_or(0, |i| i + 1)..].to_string();
                (name, v.scope.clone(), v.refers_to.clone())
            })
            .collect()
    }

    pub fn clear_names(&mut self) {
        self.defined_names.clear();
    }

    // ── 绘制抑制（惰性计数）─────────────────────────────
    pub fn suspend_paint(&mut self) {
        self.paint_suspend += 1;
    }

    pub fn resume_paint(&mut self) {
        if self.paint_suspend > 0 {
            self.paint_suspend -= 1;
        }
    }

    pub fn is_paint_suspended(&self) -> bool {
        self.paint_suspend > 0
    }

    // ── 视口态（M9 冻结）────────────────────────────────
    /// 读视口态（冻结/尾冻结/拆分）。
    pub fn viewport(&self) -> ViewportState {
        self.viewport
    }

    /// 设整个视口态。
    pub fn set_viewport(&mut self, vp: ViewportState) {
        self.viewport = vp;
    }

    /// 冻结窗格（顶 rows 行 + 左 cols 列）。对齐 element freeze()。
    pub fn freeze_panes(&mut self, rows: u32, cols: u32) {
        self.viewport.frozen_row_count = rows;
        self.viewport.frozen_col_count = cols;
    }

    /// 拆分模式冻结（冻结线可拖拽）。
    pub fn split_panes(&mut self, rows: u32, cols: u32) {
        self.viewport.frozen_row_count = rows;
        self.viewport.frozen_col_count = cols;
        self.viewport.split_row = rows > 0;
        self.viewport.split_col = cols > 0;
    }

    /// 解冻（清冻结 + 拆分）。
    pub fn unfreeze_panes(&mut self) {
        self.viewport.frozen_row_count = 0;
        self.viewport.frozen_col_count = 0;
        self.viewport.split_row = false;
        self.viewport.split_col = false;
    }
}

fn name_key(name: &str, scope: &str) -> String {
    format!("{} {}", scope, name.to_uppercase())
}

/// 移动 sheet 后，活动索引的新位置（按被移动元素身份跟随）。
fn reindex_after_move(active: usize, from: usize, dest: usize) -> usize {
    if active == from {
        return dest;
    }
    // 先删 from，再插 dest，推算 active 的漂移
    let mut a = active;
    if active > from {
        a -= 1;
    }
    if a >= dest {
        a += 1;
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn creates_requested_sheets() {
        let wb = Workbook::with_sheets(3);
        assert_eq!(wb.sheet_count(), 3);
        assert_eq!(wb.sheet(0).unwrap().name(), "Sheet1");
        assert_eq!(wb.active_sheet_index(), 0);
    }

    #[test]
    fn defaults_single_sheet() {
        assert_eq!(Workbook::default().sheet_count(), 1);
    }

    #[test]
    fn add_sheet_shifts_active() {
        let mut wb = Workbook::with_sheets(2);
        wb.set_active_sheet_index(1);
        wb.add_sheet(0, Worksheet::new("Inserted"));
        assert_eq!(wb.sheet_count(), 3);
        assert_eq!(wb.sheet(0).unwrap().name(), "Inserted");
        assert_eq!(wb.active_sheet_index(), 2);
    }

    #[test]
    fn append_adds_at_end() {
        let mut wb = Workbook::with_sheets(1);
        let idx = wb.append_new_sheet();
        assert_eq!(wb.sheet_count(), 2);
        assert_eq!(idx, 1);
    }

    #[test]
    fn remove_clamps_active() {
        let mut wb = Workbook::with_sheets(2);
        wb.set_active_sheet_index(1);
        wb.remove_sheet(1);
        assert_eq!(wb.sheet_count(), 1);
        assert_eq!(wb.active_sheet_index(), 0);
    }

    #[test]
    fn clear_sheets_empties() {
        let mut wb = Workbook::with_sheets(3);
        wb.clear_sheets();
        assert_eq!(wb.sheet_count(), 0);
        assert!(wb.active_sheet().is_none());
    }

    #[test]
    fn finds_by_name() {
        let mut wb = Workbook::empty();
        wb.append_sheet(Worksheet::new("资产负债表"));
        assert!(wb.sheet_by_name("资产负债表").is_some());
        assert!(wb.sheet_by_name("nope").is_none());
    }

    fn named_abc() -> Workbook {
        let mut wb = Workbook::empty();
        for n in ["A", "B", "C"] {
            wb.append_sheet(Worksheet::new(n));
        }
        wb.set_active_sheet_index(0);
        wb
    }
    fn order(wb: &Workbook) -> String {
        wb.sheets().iter().map(|s| s.name()).collect()
    }

    #[test]
    fn move_forward() {
        let mut wb = named_abc();
        wb.move_sheet(0, 2);
        assert_eq!(order(&wb), "BCA");
    }

    #[test]
    fn move_backward() {
        let mut wb = named_abc();
        wb.move_sheet(2, 0);
        assert_eq!(order(&wb), "CAB");
    }

    #[test]
    fn move_keeps_active_selected() {
        let mut wb = named_abc();
        wb.set_active_sheet_index(0); // active = A
        wb.move_sheet(2, 0); // C to front → A now at index 1
        assert_eq!(wb.active_sheet().unwrap().name(), "A");
        assert_eq!(wb.active_sheet_index(), 1);
    }

    #[test]
    fn move_noop_same_index() {
        let mut wb = named_abc();
        wb.move_sheet(1, 1);
        assert_eq!(order(&wb), "ABC");
        wb.move_sheet(0, 99); // clamps to end
        assert_eq!(order(&wb), "BCA");
    }

    #[test]
    fn emits_active_sheet_changed() {
        let mut wb = Workbook::with_sheets(3);
        let seen = Rc::new(RefCell::new(Vec::<(usize, usize)>::new()));
        let seen2 = seen.clone();
        wb.bind(
            WorkbookEventKind::ActiveSheetChanged,
            Box::new(move |ev| {
                if let WorkbookEvent::ActiveSheetChanged {
                    old_index,
                    new_index,
                    ..
                } = ev
                {
                    seen2.borrow_mut().push((*old_index, *new_index));
                }
            }),
        );
        wb.set_active_sheet_index(2);
        assert_eq!(&*seen.borrow(), &[(0, 2)]);
    }

    #[test]
    fn no_emit_when_unchanged() {
        let mut wb = Workbook::with_sheets(2);
        let count = Rc::new(Cell::new(0));
        let c2 = count.clone();
        wb.bind(
            WorkbookEventKind::ActiveSheetChanged,
            Box::new(move |_| c2.set(c2.get() + 1)),
        );
        wb.set_active_sheet_index(0);
        assert_eq!(count.get(), 0);
    }

    #[test]
    fn emits_added_removed() {
        let mut wb = Workbook::with_sheets(1);
        let added = Rc::new(Cell::new(0));
        let removed = Rc::new(Cell::new(0));
        let a2 = added.clone();
        let r2 = removed.clone();
        wb.bind(
            WorkbookEventKind::SheetAdded,
            Box::new(move |_| a2.set(a2.get() + 1)),
        );
        wb.bind(
            WorkbookEventKind::SheetRemoved,
            Box::new(move |_| r2.set(r2.get() + 1)),
        );
        wb.append_sheet(Worksheet::new("X"));
        wb.remove_sheet(1);
        assert_eq!(added.get(), 1);
        assert_eq!(removed.get(), 1);
    }

    #[test]
    fn undo_do_and_undo() {
        let wb = Workbook::default();
        let um = wb.undo_manager();
        let state = Rc::new(Cell::new(0));
        let s1 = state.clone();
        let s2 = state.clone();
        um.borrow_mut().do_action(Box::new(ClosureAction::new(
            "inc",
            move || s1.set(s1.get() + 1),
            move || s2.set(s2.get() - 1),
        )));
        assert_eq!(state.get(), 1);
        assert!(um.borrow().can_undo());
        um.borrow_mut().undo();
        assert_eq!(state.get(), 0);
        assert!(um.borrow().can_redo());
        um.borrow_mut().redo();
        assert_eq!(state.get(), 1);
    }

    #[test]
    fn new_action_clears_redo() {
        let wb = Workbook::default();
        let um = wb.undo_manager();
        um.borrow_mut()
            .do_action(Box::new(ClosureAction::new("a", || {}, || {})));
        um.borrow_mut().undo();
        assert!(um.borrow().can_redo());
        um.borrow_mut()
            .do_action(Box::new(ClosureAction::new("b", || {}, || {})));
        assert!(!um.borrow().can_redo());
    }

    #[test]
    fn respects_max_size() {
        let wb = Workbook::default();
        let um = wb.undo_manager();
        um.borrow_mut().set_max_size(2);
        for i in 0..5 {
            um.borrow_mut()
                .do_action(Box::new(ClosureAction::new(format!("a{i}"), || {}, || {})));
        }
        assert_eq!(um.borrow().undo_stack_len(), 2);
    }

    #[test]
    fn clear_empties_stacks() {
        let wb = Workbook::default();
        let um = wb.undo_manager();
        um.borrow_mut()
            .do_action(Box::new(ClosureAction::new("a", || {}, || {})));
        um.borrow_mut().clear();
        assert!(!um.borrow().can_undo());
        assert!(!um.borrow().can_redo());
    }

    #[test]
    fn command_register_execute_undo() {
        let mut wb = Workbook::with_sheets(1);
        let log = Rc::new(RefCell::new(Vec::<String>::new()));
        let log_do = log.clone();
        let log_undo = log.clone();
        let cmd = Command {
            can_undo: true,
            execute: Box::new(move |opts| {
                log_do.borrow_mut().push(format!(
                    "do:{}",
                    opts.args.get("tag").cloned().unwrap_or_default()
                ));
            }),
            undo: Some(Box::new(move |opts| {
                log_undo.borrow_mut().push(format!(
                    "undo:{}",
                    opts.args.get("tag").cloned().unwrap_or_default()
                ));
            })),
        };
        wb.command_manager().register("tagged", cmd);
        let ok = wb
            .command_manager()
            .execute(CommandOptions::new("tagged").arg("tag", "X").named("打标"));
        assert!(ok);
        assert_eq!(&*log.borrow(), &["do:X".to_string()]);
        assert!(wb.can_undo());
        wb.undo();
        assert_eq!(&*log.borrow(), &["do:X".to_string(), "undo:X".to_string()]);
    }

    #[test]
    fn command_unknown_returns_false() {
        let mut wb = Workbook::default();
        assert!(!wb.command_manager().execute(CommandOptions::new("ghost")));
    }

    #[test]
    fn paint_suspend_nests() {
        let mut wb = Workbook::default();
        wb.suspend_paint();
        wb.suspend_paint();
        assert!(wb.is_paint_suspended());
        wb.resume_paint();
        assert!(wb.is_paint_suspended());
        wb.resume_paint();
        assert!(!wb.is_paint_suspended());
        wb.resume_paint(); // underflow safe
        assert!(!wb.is_paint_suspended());
    }

    #[test]
    fn defined_names_scope() {
        let mut wb = Workbook::empty();
        wb.append_sheet(Worksheet::new("Sheet1"));
        wb.define_name("Tax", "Sheet1!$A$1", "workbook");
        wb.define_name("Tax", "Sheet1!$B$2", "Sheet1");
        // sheet 级优先
        assert_eq!(
            wb.resolve_name("tax", Some("Sheet1")).as_deref(),
            Some("Sheet1!$B$2")
        );
        // 无 sheet 作用域 → workbook 级
        assert_eq!(wb.resolve_name("TAX", None).as_deref(), Some("Sheet1!$A$1"));
        assert_eq!(wb.list_names().len(), 2);
    }

    #[test]
    fn viewport_freeze_state() {
        let mut wb = Workbook::default();
        assert_eq!(wb.viewport(), ViewportState::default());
        wb.freeze_panes(2, 1);
        assert_eq!(wb.viewport().frozen_row_count, 2);
        assert_eq!(wb.viewport().frozen_col_count, 1);
        assert!(!wb.viewport().split_row);
        wb.split_panes(3, 0);
        assert!(wb.viewport().split_row);
        assert!(!wb.viewport().split_col);
        wb.unfreeze_panes();
        assert_eq!(wb.viewport(), ViewportState::default());
    }

    #[test]
    fn is_frozen_predicate() {
        assert!(is_frozen(0, 2));
        assert!(is_frozen(1, 2));
        assert!(!is_frozen(2, 2));
        assert!(!is_frozen(0, 0));
    }
}
