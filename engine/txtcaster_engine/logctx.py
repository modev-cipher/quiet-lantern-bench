import contextvars
import logging

job_id_var: contextvars.ContextVar[str] = contextvars.ContextVar("job_id", default="-")


class JobIdFilter(logging.Filter):
    def filter(self, record: logging.LogRecord) -> bool:
        record.job = job_id_var.get()
        return True
