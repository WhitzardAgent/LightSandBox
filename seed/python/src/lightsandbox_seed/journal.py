"""SQLite idempotency/CAS reference implementation; no process management.

Use only in a trusted daemon-owned state directory. A recorded intent does not
make external side effects atomic. UNKNOWN is intentionally never auto-replayed.
This Python seed specifies behavior to port to the Rust single-writer actor.
"""
from __future__ import annotations
from dataclasses import dataclass
import hashlib,json,sqlite3,uuid
from pathlib import Path

TRANSITIONS={
 'PREPARED':frozenset(('RUNNING','FAILED')),
 'RUNNING':frozenset(('SUCCEEDED','FAILED','UNKNOWN')),
 'UNKNOWN':frozenset(), 'FAILED':frozenset(), 'SUCCEEDED':frozenset(),
}
@dataclass(frozen=True)
class Operation:
    operation_id:str
    status:str
    revision:int
    result:dict|None
    new:bool=False

class Journal:
    def __init__(self,path:Path):
        self.conn=sqlite3.connect(Path(path),timeout=5,isolation_level=None)
        self.conn.row_factory=sqlite3.Row
        self.conn.execute('PRAGMA journal_mode=WAL');self.conn.execute('PRAGMA synchronous=FULL')
        self.conn.execute('PRAGMA foreign_keys=ON')
        self.conn.execute('''CREATE TABLE IF NOT EXISTS operations (
          operation_id TEXT PRIMARY KEY, owner TEXT NOT NULL, key TEXT NOT NULL,
          fingerprint TEXT NOT NULL, status TEXT NOT NULL CHECK(status IN ('PREPARED','RUNNING','SUCCEEDED','FAILED','UNKNOWN')),
          revision INTEGER NOT NULL CHECK(revision>=0), result TEXT,
          UNIQUE(owner,key))''')
    def close(self):self.conn.close()
    @staticmethod
    def _row(row,new=False):return Operation(row['operation_id'],row['status'],row['revision'],json.loads(row['result']) if row['result'] is not None else None,new)
    def begin(self,owner:str,key:str,payload:dict)->Operation:
        if not isinstance(payload,dict):raise ValueError('payload must be object')
        if not all(isinstance(s,str) and 0<len(s)<=256 and '\x00' not in s for s in (owner,key)):raise ValueError('invalid owner/idempotency key')
        encoded=json.dumps(payload,sort_keys=True,separators=(',',':'),allow_nan=False).encode()
        fingerprint=hashlib.sha256(encoded).hexdigest()
        self.conn.execute('BEGIN IMMEDIATE')
        try:
            row=self.conn.execute('SELECT * FROM operations WHERE owner=? AND key=?',(owner,key)).fetchone()
            if row:
                if row['fingerprint']!=fingerprint:raise ValueError('idempotency conflict')
                answer=self._row(row)
            else:
                op=uuid.uuid4().hex
                self.conn.execute('INSERT INTO operations VALUES(?,?,?,?,?,?,?)',(op,owner,key,fingerprint,'PREPARED',0,None))
                answer=self._row(self.conn.execute('SELECT * FROM operations WHERE operation_id=?',(op,)).fetchone(),True)
            self.conn.execute('COMMIT');return answer
        except BaseException:
            self.conn.execute('ROLLBACK');raise
    def transition(self,operation_id:str,revision:int,target:str,result:dict|None=None)->Operation:
        if type(revision) is not int or revision<0:raise ValueError('invalid revision')
        encoded=json.dumps(result,sort_keys=True,allow_nan=False) if result is not None else None
        self.conn.execute('BEGIN IMMEDIATE')
        try:
            row=self.conn.execute('SELECT * FROM operations WHERE operation_id=?',(operation_id,)).fetchone()
            if row is None:raise ValueError('operation not found')
            if row['revision']!=revision:raise ValueError('stale revision')
            if target not in TRANSITIONS[row['status']]:raise ValueError('illegal state transition')
            if target=='SUCCEEDED' and not isinstance(result,dict):raise ValueError('success requires result')
            self.conn.execute('UPDATE operations SET status=?,revision=revision+1,result=? WHERE operation_id=? AND revision=?',(target,encoded,operation_id,revision))
            answer=self._row(self.conn.execute('SELECT * FROM operations WHERE operation_id=?',(operation_id,)).fetchone())
            self.conn.execute('COMMIT');return answer
        except BaseException:
            self.conn.execute('ROLLBACK');raise
    def reconcile_lost_processes(self)->int:
        """Conservative fallback ONLY after broker/runtime reconciliation failed.

        A real runtime first asks its surviving brokers for an authoritative exit
        receipt; this helper never equates daemon restart with successful exit.
        """
        return self.conn.execute("UPDATE operations SET status='UNKNOWN',revision=revision+1 WHERE status='RUNNING'").rowcount
