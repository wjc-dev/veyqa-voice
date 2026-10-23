"""音频处理模块
提供音频录制和处理功能
"""

__all__ = ['AudioRecorder']


def __getattr__(name):
    # Importing the disposable worker must not load NumPy, libsndfile or logs.
    if name == "AudioRecorder":
        from .recorder import AudioRecorder
        return AudioRecorder
    raise AttributeError(name)
