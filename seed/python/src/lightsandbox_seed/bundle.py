"""Bounded, read-only environment capture. NOT an OCI layer unpacker.

Portable seed deliberately rejects symlinks and special files. It fails explicitly
rather than weakening the production parity requirement. Intended input is an
administrator-approved local task tree; no host extraction primitive is exposed.
"""
from __future__ import annotations
from dataclasses import dataclass, asdict
import hashlib, io, json, os, stat, tarfile
from pathlib import Path, PurePosixPath

@dataclass(frozen=True)
class Entry:
    path: str
    kind: str
    mode: int
    size: int
    sha256: str | None

@dataclass(frozen=True)
class Snapshot:
    digest: str
    entries: tuple[Entry, ...]
    total_bytes: int

def validate_relative_path(value: str) -> str:
    if not isinstance(value,str) or not value or '\x00' in value or '\\' in value:
        raise ValueError('invalid relative path')
    if value.startswith('/') or any(x in ('','.','..') for x in value.split('/')):
        raise ValueError('unsafe relative path')
    if PurePosixPath(value).as_posix()!=value:
        raise ValueError('noncanonical relative path')
    return value

def _capture(root: Path, *, max_entries: int=10000, max_file_bytes: int=32*1024*1024,
             max_total_bytes: int=64*1024*1024, max_depth: int=64):
    for v in (max_entries,max_file_bytes,max_total_bytes,max_depth):
        if type(v) is not int or v<1:raise ValueError('invalid capture budget')
    root=Path(root)
    if root.is_symlink():raise ValueError('symlink source root not supported by seed')
    flags=os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW|os.O_CLOEXEC
    fd=os.open(root,flags);entries=[];blobs={};total=0
    def walk(dirfd:int, prefix:str, depth:int):
        nonlocal total
        if depth>max_depth:raise ValueError('directory depth budget exceeded')
        for name in sorted(os.listdir(dirfd)):
            rel=f'{prefix}/{name}' if prefix else name;validate_relative_path(rel)
            if len(entries)>=max_entries:raise ValueError('entry budget exceeded')
            before=os.stat(name,dir_fd=dirfd,follow_symlinks=False)
            if stat.S_ISLNK(before.st_mode):raise ValueError(f'symlink requires certified importer: {rel}')
            if stat.S_ISDIR(before.st_mode):
                sub=os.open(name,flags,dir_fd=dirfd)
                try:
                    now=os.fstat(sub)
                    if (now.st_dev,now.st_ino)!=(before.st_dev,before.st_ino):raise ValueError('source changed during capture')
                    entries.append(Entry(rel,'directory',stat.S_IMODE(now.st_mode),0,None))
                    walk(sub,rel,depth+1)
                finally:os.close(sub)
            elif stat.S_ISREG(before.st_mode):
                f=os.open(name,os.O_RDONLY|os.O_NOFOLLOW|os.O_CLOEXEC|os.O_NONBLOCK,dir_fd=dirfd)
                try:
                    first=os.fstat(f)
                    if not stat.S_ISREG(first.st_mode) or (first.st_dev,first.st_ino)!=(before.st_dev,before.st_ino):
                        raise ValueError('source file changed during capture')
                    if first.st_size>max_file_bytes:raise ValueError('file byte budget exceeded')
                    chunks=[];size=0
                    while True:
                        b=os.read(f,min(65536,max_file_bytes-size+1))
                        if not b:break
                        size+=len(b);total+=len(b)
                        if size>max_file_bytes or total>max_total_bytes:raise ValueError('capture byte budget exceeded')
                        chunks.append(b)
                    last=os.fstat(f)
                    if (first.st_size,first.st_mtime_ns,first.st_ctime_ns)!=(last.st_size,last.st_mtime_ns,last.st_ctime_ns):
                        raise ValueError('source file changed during capture')
                    data=b''.join(chunks);blobs[rel]=data
                    entries.append(Entry(rel,'file',stat.S_IMODE(first.st_mode),size,hashlib.sha256(data).hexdigest()))
                finally:os.close(f)
            else:raise ValueError(f'special file requires explicit importer policy: {rel}')
    try:walk(fd,'',0)
    finally:os.close(fd)
    entries.sort(key=lambda e:e.path)
    identity=json.dumps([asdict(e) for e in entries],sort_keys=True,separators=(',',':'),ensure_ascii=False).encode()
    return Snapshot(hashlib.sha256(identity).hexdigest(),tuple(entries),total),blobs

def snapshot_tree(root:Path,**budgets)->Snapshot:
    return _capture(root,**budgets)[0]

def export_bundle(root:Path,**budgets)->bytes:
    """Create bounded deterministic transport tar from captured bytes; no extraction.

    No uid/gid/mtime-based OCI equivalence is claimed. Actual build cache keys must
    also bind Dockerfile, frontend, image digests, build args and target platform.
    """
    snapshot,blobs=_capture(root,**budgets);out=io.BytesIO()
    with tarfile.open(fileobj=out,mode='w',format=tarfile.PAX_FORMAT) as tf:
        for e in snapshot.entries:
            info=tarfile.TarInfo(e.path);info.mode=e.mode;info.uid=info.gid=0;info.mtime=0
            if e.kind=='directory':info.type=tarfile.DIRTYPE;tf.addfile(info)
            else:info.size=e.size;tf.addfile(info,io.BytesIO(blobs[e.path]))
    return out.getvalue()

def read_regular_file(path:Path,limit:int=1024*1024)->bytes:
    if type(limit) is not int or limit<1:raise ValueError('invalid file limit')
    fd=os.open(Path(path),os.O_RDONLY|os.O_NOFOLLOW|os.O_CLOEXEC|os.O_NONBLOCK)
    try:
        before=os.fstat(fd)
        if not stat.S_ISREG(before.st_mode) or before.st_size>limit:raise ValueError('not a bounded regular file')
        with os.fdopen(os.dup(fd),'rb') as f:data=f.read(limit+1)
        after=os.fstat(fd)
        if len(data)>limit or (before.st_ino,before.st_size,before.st_mtime_ns,before.st_ctime_ns)!=(after.st_ino,after.st_size,after.st_mtime_ns,after.st_ctime_ns):
            raise ValueError('file changed or exceeded limit')
        return data
    except OSError as exc:raise ValueError('cannot safely read file') from exc
    finally:os.close(fd)
