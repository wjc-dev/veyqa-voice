"""Compatibility settings UI for the voice-input service."""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
from pathlib import Path
from urllib.parse import urlparse

from dotenv import dotenv_values
from PyQt5.QtCore import Qt, QUrl
from PyQt5.QtGui import QDesktopServices
from PyQt5.QtWidgets import (
    QAbstractItemView,
    QApplication,
    QCheckBox,
    QComboBox,
    QDialog,
    QDialogButtonBox,
    QFormLayout,
    QGroupBox,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QLineEdit,
    QMessageBox,
    QPushButton,
    QTabWidget,
    QTableWidget,
    QTableWidgetItem,
    QTextEdit,
    QVBoxLayout,
    QWidget,
)

ROOT_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT_DIR))
from src.correction_learning import CorrectionStore
ENV_PATH = ROOT_DIR / ".env"
LAUNCH_AGENT_LABEL = "com.whisper-input-next"
CONTEXT_PATH = ROOT_DIR / "personal_context.txt"

SERVICE_OPTIONS = [
    ("千问 Qwen Audio 3.0 ASR", "qwen"),
]
PUNCTUATION_OPTIONS = [
    ("保留语音服务自动标点", "auto"),
    ("标点替换为空格，保留问号", "spaces"),
    ("删除所有标点", "none"),
]
QWEN_REGION_OPTIONS = [
    ("中国内地（北京）", "beijing"),
    ("新加坡", "singapore"),
]
QWEN_LANGUAGE_OPTIONS = [
    ("中文优先", "zh"),
    ("自动判断语言", "auto"),
    ("粤语", "yue"),
    ("英语", "en"),
]
FN_MODE_OPTIONS = [
    ("按住 Fn 录音，松开结束", "hold"),
    ("按一下开始，再按一下结束", "toggle"),
]
HOTKEY_BACKEND_OPTIONS = [
    ("系统注册 ⌃⌥Space（无需输入监控）", "registered"),
    ("只读键盘监听", "passive"),
    ("关闭全局快捷键", "off"),
]
DISFLUENCY_OPTIONS = [
    ("关闭（原样识别）", "off"),
    ("仅去除“嗯、呃、啊”", "fillers"),
    ("口头语 + 明显重复（实验性）", "conservative"),
]


def update_env_file(path: Path, updates: dict[str, str]) -> None:
    """Update selected .env values atomically while preserving other lines."""
    existing_lines = path.read_text(encoding="utf-8").splitlines()
    pending = dict(updates)
    updated_lines: list[str] = []

    for line in existing_lines:
        stripped = line.strip()
        if stripped and not stripped.startswith("#") and "=" in line:
            key = line.split("=", 1)[0].strip()
            if key in pending:
                updated_lines.append(f"{key}={pending.pop(key)}")
                continue
        updated_lines.append(line)

    if pending:
        if updated_lines and updated_lines[-1] != "":
            updated_lines.append("")
        for key, value in pending.items():
            updated_lines.append(f"{key}={value}")

    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            handle.write("\n".join(updated_lines) + "\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary_name, 0o600)
        os.replace(temporary_name, path)
    finally:
        if os.path.exists(temporary_name):
            os.unlink(temporary_name)


def write_private_text_file(path: Path, content: str) -> None:
    """Atomically write a user-owned private text file."""
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            handle.write(content)
            if content and not content.endswith("\n"):
                handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary_name, 0o600)
        os.replace(temporary_name, path)
    finally:
        if os.path.exists(temporary_name):
            os.unlink(temporary_name)


def help_text(text: str) -> QLabel:
    label = QLabel(text)
    label.setWordWrap(True)
    label.setObjectName("helpText")
    return label


def section(title: str, description: str = "") -> tuple[QGroupBox, QVBoxLayout]:
    box = QGroupBox(title)
    layout = QVBoxLayout(box)
    layout.setContentsMargins(18, 22, 18, 18)
    layout.setSpacing(12)
    if description:
        layout.addWidget(help_text(description))
    return box, layout


class ContextEditorDialog(QDialog):
    """Large editor kept outside the main settings layout."""

    def __init__(self, text: str, parent: QWidget | None = None) -> None:
        super().__init__(parent)
        self.setWindowTitle("编辑千问用户词汇与上下文")
        self.resize(720, 500)

        title = QLabel("用户词汇与上下文")
        title.setObjectName("dialogTitle")
        note = help_text(
            "这里适合填写职业背景、项目名、人名和常用术语。内容仅在启用后随千问语音请求发送；"
            "自动纠错词库不需要在这里重复填写。"
        )
        self.editor = QTextEdit()
        self.editor.setPlainText(text)
        self.editor.setPlaceholderText(
            "例如：\n"
            "工作领域：机器学习建模。\n"
            "常用术语：LightGBM、trade-off。\n"
            "请只填写你自己希望发送给语音服务的内容。"
        )
        buttons = QDialogButtonBox(QDialogButtonBox.Cancel | QDialogButtonBox.Save)
        buttons.button(QDialogButtonBox.Save).setText("保存内容")
        buttons.button(QDialogButtonBox.Cancel).setText("取消")
        buttons.accepted.connect(self.accept)
        buttons.rejected.connect(self.reject)

        layout = QVBoxLayout(self)
        layout.setContentsMargins(22, 22, 22, 18)
        layout.setSpacing(12)
        layout.addWidget(title)
        layout.addWidget(note)
        layout.addWidget(self.editor, 1)
        layout.addWidget(buttons)

    def text(self) -> str:
        return self.editor.toPlainText().strip()


class SettingsWindow(QWidget):
    def __init__(self) -> None:
        super().__init__()
        self.setWindowTitle("语音输入设置")
        self.resize(780, 720)
        self.setMinimumSize(700, 640)
        self.setAttribute(Qt.WA_MacShowFocusRect, False)

        values = dotenv_values(ENV_PATH)
        self.context_text = ""
        if CONTEXT_PATH.exists():
            try:
                self.context_text = CONTEXT_PATH.read_text(encoding="utf-8").strip()
            except OSError:
                pass

        self._build_controls(values)
        self._build_layout()
        self._apply_style()
        self._sync_service_summary()
        self._update_context_summary()
        self._load_correction_rules()

    def _build_controls(self, values: dict) -> None:
        self.service_combo = QComboBox()
        for label, value in SERVICE_OPTIONS:
            self.service_combo.addItem(label, value)
        self._select_value(self.service_combo, "qwen")
        self.service_combo.currentIndexChanged.connect(self._sync_service_summary)

        self.punctuation_combo = QComboBox()
        for label, value in PUNCTUATION_OPTIONS:
            self.punctuation_combo.addItem(label, value)
        self._select_value(self.punctuation_combo, values.get("PUNCTUATION_MODE") or "auto")
        self.disfluency_combo = QComboBox()
        for label, value in DISFLUENCY_OPTIONS:
            self.disfluency_combo.addItem(label, value)
        legacy_enabled = (values.get("DISFLUENCY_FILTER_ENABLED") or "false").lower() == "true"
        self._select_value(
            self.disfluency_combo,
            values.get("DISFLUENCY_FILTER_MODE") or ("conservative" if legacy_enabled else "off"),
        )

        self.fn_mode_combo = QComboBox()
        for label, value in FN_MODE_OPTIONS:
            self.fn_mode_combo.addItem(label, value)
        self._select_value(self.fn_mode_combo, values.get("FN_HOTKEY_MODE") or "hold")
        self.hotkey_backend_combo = QComboBox()
        for label, value in HOTKEY_BACKEND_OPTIONS:
            self.hotkey_backend_combo.addItem(label, value)
        self._select_value(
            self.hotkey_backend_combo,
            values.get("GLOBAL_HOTKEY_BACKEND") or "passive",
        )

        self.qwen_api_key_input = QLineEdit(values.get("QWEN_API_KEY") or "")
        self.qwen_api_key_input.setPlaceholderText("百炼 API Key，例如 sk-… 或 sk-ws-…")
        self.qwen_api_key_input.setEchoMode(QLineEdit.Password)
        self.show_qwen_key_checkbox = QCheckBox("显示 API Key")
        self.show_qwen_key_checkbox.toggled.connect(
            lambda visible: self.qwen_api_key_input.setEchoMode(
                QLineEdit.Normal if visible else QLineEdit.Password
            )
        )
        self.qwen_api_host_input = QLineEdit(values.get("QWEN_API_HOST") or "")
        self.qwen_api_host_input.setPlaceholderText("API Key 页面上的 OpenAI compatible 地址")
        self.qwen_api_host_input.setCursorPosition(0)
        self.qwen_workspace_input = QLineEdit(values.get("QWEN_WORKSPACE_ID") or "")
        self.qwen_workspace_input.setPlaceholderText("通常留空；不要填写 API Key 列表中的数字 ID")
        self.qwen_workspace_input.setCursorPosition(0)

        self.qwen_region_combo = QComboBox()
        for label, value in QWEN_REGION_OPTIONS:
            self.qwen_region_combo.addItem(label, value)
        self._select_value(self.qwen_region_combo, values.get("QWEN_REGION") or "beijing")
        self.qwen_language_combo = QComboBox()
        for label, value in QWEN_LANGUAGE_OPTIONS:
            self.qwen_language_combo.addItem(label, value)
        self._select_value(self.qwen_language_combo, values.get("QWEN_LANGUAGE") or "zh")

        self.context_checkbox = QCheckBox("启用用户词汇与上下文")
        self.context_checkbox.setChecked(
            (values.get("QWEN_CONTEXT_ENABLED") or "false").lower() == "true"
        )
        self.context_summary = QLabel()
        self.context_summary.setObjectName("summaryBadge")
        self.edit_context_button = QPushButton("编辑…")
        self.edit_context_button.clicked.connect(self._edit_context)
        self.recent_memory_checkbox = QCheckBox("使用最近 20 条本工具转写作为近期主题（实验性）")
        self.recent_memory_checkbox.setChecked(
            (values.get("QWEN_RECENT_MEMORY_ENABLED") or "false").lower() == "true"
        )

        self.learning_checkbox = QCheckBox("自动检测语音插入后的人工修改")
        self.learning_checkbox.setChecked(
            (values.get("CORRECTION_LEARNING_ENABLED") or "true").lower() == "true"
        )
        self.auto_replace_checkbox = QCheckBox("下次识别到相同错误时自动替换")
        self.auto_replace_checkbox.setChecked(
            (values.get("CORRECTION_AUTO_REPLACE") or "true").lower() == "true"
        )
        self.learned_context_checkbox = QCheckBox("高频纠错额外提示千问识别（累计 2 次）")
        self.learned_context_checkbox.setChecked(
            (values.get("CORRECTION_CONTEXT_ENABLED") or "true").lower() == "true"
        )

        self.correction_store = CorrectionStore()
        self.correction_table = QTableWidget(0, 5)
        self.correction_table.setHorizontalHeaderLabels(
            ["排名", "识别错误", "人工改为", "次数", "最近纠正"]
        )
        self.correction_table.setEditTriggers(QAbstractItemView.NoEditTriggers)
        self.correction_table.setSelectionBehavior(QAbstractItemView.SelectRows)
        self.correction_table.setSelectionMode(QAbstractItemView.SingleSelection)
        self.correction_table.verticalHeader().setVisible(False)
        self.correction_table.setAlternatingRowColors(True)
        header = self.correction_table.horizontalHeader()
        header.setSectionResizeMode(0, QHeaderView.ResizeToContents)
        header.setSectionResizeMode(1, QHeaderView.Stretch)
        header.setSectionResizeMode(2, QHeaderView.Stretch)
        header.setSectionResizeMode(3, QHeaderView.ResizeToContents)
        header.setSectionResizeMode(4, QHeaderView.ResizeToContents)

    def _build_layout(self) -> None:
        title = QLabel("语音输入设置")
        title.setObjectName("pageTitle")
        self.service_summary = QLabel()
        self.service_summary.setObjectName("serviceSummary")

        tabs = QTabWidget()
        tabs.setDocumentMode(True)
        tabs.addTab(self._general_tab(), "通用")
        tabs.addTab(self._qwen_tab(), "千问")

        save_button = QPushButton("保存并重启")
        save_button.setObjectName("primaryButton")
        save_button.setDefault(True)
        save_button.clicked.connect(self._save_and_restart)
        close_button = QPushButton("关闭")
        close_button.clicked.connect(self.close)

        footer = QHBoxLayout()
        footer_note = help_text("配置和凭证仅保存在本机。")
        footer_note.setMinimumWidth(190)
        footer.addWidget(footer_note)
        footer.addStretch(1)
        footer.addWidget(close_button)
        footer.addWidget(save_button)

        layout = QVBoxLayout(self)
        layout.setContentsMargins(24, 20, 24, 18)
        layout.setSpacing(14)
        heading = QHBoxLayout()
        heading.addWidget(title)
        heading.addStretch(1)
        heading.addWidget(self.service_summary)
        layout.addLayout(heading)
        layout.addWidget(tabs, 1)
        layout.addLayout(footer)

    def _general_tab(self) -> QWidget:
        tab = QWidget()
        box, layout = section(
            "基础设置",
            "千问语音输入的快捷键与文字输出设置。",
        )
        form = QFormLayout()
        form.setHorizontalSpacing(22)
        form.setVerticalSpacing(16)
        form.setLabelAlignment(Qt.AlignRight | Qt.AlignVCenter)
        form.addRow("当前语音引擎", self.service_combo)
        form.addRow("标点模式", self.punctuation_combo)
        form.addRow("口语清理", self.disfluency_combo)
        form.addRow("Fn 操作", self.fn_mode_combo)
        form.addRow("键盘兼容性", self.hotkey_backend_combo)
        layout.addLayout(form)
        layout.addStretch(1)

        page = QVBoxLayout(tab)
        page.setContentsMargins(18, 20, 18, 18)
        page.addWidget(box)
        page.addStretch(1)
        return tab

    def _qwen_tab(self) -> QWidget:
        tab = QWidget()
        credentials, credentials_layout = section(
            "千问语音服务",
            "这里配置阿里云百炼 Qwen Audio 3.0 ASR。API Host 通常比 Workspace ID 更直接。",
        )
        form = QFormLayout()
        form.setHorizontalSpacing(22)
        form.setVerticalSpacing(11)
        form.setLabelAlignment(Qt.AlignRight | Qt.AlignVCenter)
        form.addRow("API Key", self.qwen_api_key_input)
        form.addRow("", self.show_qwen_key_checkbox)
        form.addRow("API Host", self.qwen_api_host_input)
        form.addRow("Workspace ID", self.qwen_workspace_input)
        form.addRow("地域", self.qwen_region_combo)
        form.addRow("识别语言", self.qwen_language_combo)
        credentials_layout.addLayout(form)

        context_box, context_layout = section(
            "千问个性化",
            "长文本不会铺在设置页里。点击编辑后使用独立窗口维护。",
        )
        context_layout.addWidget(self.context_checkbox)
        context_row = QHBoxLayout()
        context_row.addWidget(self.context_summary)
        context_row.addStretch(1)
        context_row.addWidget(self.edit_context_button)
        context_layout.addLayout(context_row)
        context_layout.addWidget(self.recent_memory_checkbox)

        console = QPushButton("打开千问百炼 API Key 页面")
        console.clicked.connect(
            lambda: QDesktopServices.openUrl(
                QUrl("https://bailian.console.aliyun.com/?tab=model#/api-key")
            )
        )
        console_row = QHBoxLayout()
        console_row.addWidget(console)
        console_row.addStretch(1)
        context_layout.addLayout(console_row)

        page = QVBoxLayout(tab)
        page.setContentsMargins(18, 20, 18, 18)
        page.setSpacing(14)
        page.addWidget(credentials)
        page.addWidget(context_box)
        page.addStretch(1)
        return tab

    def _correction_tab(self) -> QWidget:
        tab = QWidget()
        controls, controls_layout = section(
            "千问自动纠错",
            "自动学习人工修改，按频率排序，并减少相同识别错误再次发生。",
        )
        controls_layout.addWidget(self.learning_checkbox)
        controls_layout.addWidget(self.auto_replace_checkbox)
        controls_layout.addWidget(self.learned_context_checkbox)
        controls_layout.addWidget(
            help_text(
                "高频纠错会在本地替换，并作为千问识别上下文发送。"
            )
        )

        table_box, table_layout = section("纠错词库", "按人工纠正次数从高到低排列。")
        table_layout.addWidget(self.correction_table, 1)
        buttons = QHBoxLayout()
        refresh = QPushButton("刷新")
        refresh.clicked.connect(self._load_correction_rules)
        delete = QPushButton("删除选中")
        delete.clicked.connect(self._delete_selected_rule)
        clear = QPushButton("清空词库")
        clear.clicked.connect(self._clear_correction_rules)
        buttons.addWidget(refresh)
        buttons.addWidget(delete)
        buttons.addWidget(clear)
        buttons.addStretch(1)
        table_layout.addLayout(buttons)

        page = QVBoxLayout(tab)
        page.setContentsMargins(18, 20, 18, 18)
        page.setSpacing(14)
        page.addWidget(controls)
        page.addWidget(table_box, 1)
        return tab

    def _apply_style(self) -> None:
        self.setStyleSheet(
            """
            QWidget { font-size: 14px; color: #202124; }
            QLabel#pageTitle { font-size: 22px; font-weight: 650; }
            QLabel#dialogTitle { font-size: 20px; font-weight: 650; }
            QLabel#helpText { color: #6b7280; font-size: 12px; }
            QLabel#serviceSummary, QLabel#summaryBadge {
                color: #475569; background: #eef2f7; border-radius: 7px;
                padding: 5px 10px;
            }
            QTabWidget::pane { border: 1px solid #d9dde3; border-radius: 10px; }
            QTabBar::tab { padding: 9px 22px; min-width: 72px; }
            QGroupBox {
                background: #ffffff; border: 1px solid #e1e4e8; border-radius: 9px;
                margin-top: 10px; font-weight: 600;
            }
            QGroupBox::title { subcontrol-origin: margin; left: 14px; padding: 0 6px; }
            QLineEdit, QComboBox, QTextEdit {
                min-height: 30px; border: 1px solid #cfd4dc; border-radius: 6px;
                padding: 2px 8px; background: #ffffff;
            }
            QLineEdit:focus, QComboBox:focus, QTextEdit:focus { border: 1px solid #2684ff; }
            QPushButton { min-height: 28px; padding: 2px 13px; }
            QPushButton#primaryButton {
                color: white; background: #1473e6; border: 1px solid #1473e6;
                border-radius: 6px; padding: 3px 17px;
            }
            QPushButton#primaryButton:hover { background: #0f65cc; }
            QTableWidget { border: 1px solid #d9dde3; border-radius: 6px; gridline-color: #edf0f3; }
            QHeaderView::section {
                background: #f4f6f8; padding: 7px; border: none;
                border-right: 1px solid #e1e4e8; font-weight: 600;
            }
            """
        )

    @staticmethod
    def _select_value(combo: QComboBox, value: str) -> None:
        index = combo.findData(value)
        if index >= 0:
            combo.setCurrentIndex(index)

    def _sync_service_summary(self) -> None:
        self.service_summary.setText("千问 Qwen Audio 3.0 ASR")

    def _update_context_summary(self) -> None:
        if not self.context_text:
            self.context_summary.setText("尚未配置内容")
            return
        line_count = len([line for line in self.context_text.splitlines() if line.strip()])
        self.context_summary.setText(f"已配置 {len(self.context_text)} 个字符 · {line_count} 行")

    def _edit_context(self) -> None:
        dialog = ContextEditorDialog(self.context_text, self)
        if dialog.exec_() == QDialog.Accepted:
            new_text = dialog.text()
            if len(new_text) > 30_000:
                QMessageBox.warning(dialog, "内容过长", "请控制在 30,000 个字符以内。")
                return
            self.context_text = new_text
            self._update_context_summary()

    def _load_correction_rules(self) -> None:
        rules = self.correction_store.rules()
        self.correction_table.setRowCount(len(rules))
        for row, rule in enumerate(rules):
            values = (
                str(row + 1),
                str(rule.get("wrong") or ""),
                str(rule.get("correct") or ""),
                str(rule.get("count") or 0),
                str(rule.get("last_seen") or "").replace("T", " ")[:19],
            )
            for column, value in enumerate(values):
                item = QTableWidgetItem(value)
                if column in (0, 3):
                    item.setTextAlignment(Qt.AlignCenter)
                self.correction_table.setItem(row, column, item)

    def _delete_selected_rule(self) -> None:
        row = self.correction_table.currentRow()
        if row < 0:
            QMessageBox.information(self, "未选择", "请先选择要删除的一条纠错。")
            return
        wrong_item = self.correction_table.item(row, 1)
        correct_item = self.correction_table.item(row, 2)
        if wrong_item and correct_item:
            self.correction_store.delete(wrong_item.text(), correct_item.text())
            self._load_correction_rules()

    def _clear_correction_rules(self) -> None:
        if not self.correction_store.rules():
            return
        answer = QMessageBox.question(
            self,
            "清空纠错词库",
            "确定删除全部自动学习记录吗？此操作不能撤销。",
            QMessageBox.Yes | QMessageBox.No,
            QMessageBox.No,
        )
        if answer == QMessageBox.Yes:
            self.correction_store.clear()
            self._load_correction_rules()

    def _validate(self) -> tuple[bool, str]:
        key = self.qwen_api_key_input.text().strip()
        if len(key) < 20 or any(character.isspace() for character in key):
            return False, "千问 API Key 格式不正确。"
        workspace = self.qwen_workspace_input.text().strip()
        if workspace and not all(character.isalnum() or character == "-" for character in workspace):
            return False, "Workspace ID 只能包含字母、数字和连字符。"
        host = self.qwen_api_host_input.text().strip()
        if host:
            candidate = host if "://" in host else "https://" + host
            if not (urlparse(candidate).hostname or "").endswith(".aliyuncs.com"):
                return False, "API Host 必须是 aliyuncs.com 官方地址。"
        if self.context_checkbox.isChecked() and len(self.context_text) > 30_000:
            return False, "用户词汇与上下文请控制在 30,000 个字符以内。"
        return True, ""

    def _save_and_restart(self) -> None:
        valid, error = self._validate()
        if not valid:
            QMessageBox.warning(self, "无法保存", error)
            return

        service = self.service_combo.currentData()
        qwen_host = self.qwen_api_host_input.text().strip()
        qwen_workspace = self.qwen_workspace_input.text().strip()
        if qwen_host:
            qwen_workspace = ""

        updates = {
            "TRANSCRIPTION_SERVICE": service,
            "PUNCTUATION_MODE": self.punctuation_combo.currentData(),
            "DISFLUENCY_FILTER_ENABLED": (
                "false" if self.disfluency_combo.currentData() == "off" else "true"
            ),
            "DISFLUENCY_FILTER_MODE": self.disfluency_combo.currentData(),
            "FN_HOTKEY_MODE": self.fn_mode_combo.currentData(),
            "GLOBAL_HOTKEY_BACKEND": self.hotkey_backend_combo.currentData(),
            "QWEN_API_KEY": self.qwen_api_key_input.text().strip(),
            "QWEN_API_HOST": qwen_host,
            "QWEN_WORKSPACE_ID": qwen_workspace,
            "QWEN_REGION": self.qwen_region_combo.currentData(),
            "QWEN_LANGUAGE": self.qwen_language_combo.currentData(),
            "QWEN_CONTEXT_ENABLED": "true" if self.context_checkbox.isChecked() else "false",
            "QWEN_CONTEXT_FILE": str(CONTEXT_PATH),
            "QWEN_RECENT_MEMORY_ENABLED": (
                "true" if self.recent_memory_checkbox.isChecked() else "false"
            ),
            "QWEN_RECENT_MEMORY_COUNT": "20",
            "EXPERIMENTAL_CORRECTION_LEARNING": "false",
            "CORRECTION_LEARNING_ENABLED": "false",
            "CORRECTION_AUTO_REPLACE": "false",
            "CORRECTION_REPLACE_MIN_COUNT": "2",
            "CORRECTION_CONTEXT_ENABLED": "false",
            "CORRECTION_CONTEXT_MIN_COUNT": "2",
        }
        if self.hotkey_backend_combo.currentData() == "registered":
            updates["VOICE_HOTKEY"] = "keycode:49;mods:control+option"
            updates["VOICE_HOTKEY_LABEL"] = "⌃⌥Space"
        try:
            update_env_file(ENV_PATH, updates)
            write_private_text_file(CONTEXT_PATH, self.context_text)
            result = subprocess.run(
                [
                    "launchctl",
                    "kickstart",
                    "-k",
                    f"gui/{os.getuid()}/{LAUNCH_AGENT_LABEL}",
                ],
                capture_output=True,
                text=True,
                timeout=15,
                check=False,
            )
            if result.returncode != 0:
                raise RuntimeError(result.stderr.strip() or "后台服务重启失败")
        except Exception as exc:  # noqa: BLE001
            QMessageBox.critical(self, "保存失败", str(exc))
            return

        QMessageBox.information(
            self,
            "设置已生效",
            "千问配置已保存，后台语音服务已经重启。",
        )


def main() -> int:
    app = QApplication(sys.argv)
    app.setApplicationName("语音输入设置")
    window = SettingsWindow()
    window.show()
    window.raise_()
    window.activateWindow()
    return app.exec_()


if __name__ == "__main__":
    raise SystemExit(main())
