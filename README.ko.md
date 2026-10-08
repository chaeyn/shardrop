# Shardrop

[![CI](https://github.com/chaeyn/shardrop/actions/workflows/ci.yml/badge.svg)](https://github.com/chaeyn/shardrop/actions/workflows/ci.yml)

폴더를 압축하고 조각내면서 전송하는 Rust CLI입니다. 전송이 끊기면 검증을 통과한 조각을 재사용합니다.

```sh
shardrop copy user@server /home/user/project -o ./project-backup
shardrop restore ./project-backup --to ./restored
```

[English](README.md) · [설계](docs/design.md) · [호환 범위](docs/compatibility.md) · [검증 기록](docs/validation.md)

## 설치

[GitHub Releases](https://github.com/chaeyn/shardrop/releases)에서 운영체제에 맞는 실행 파일을 받을 수 있습니다. 초기 버전은 사전 릴리스로 배포합니다. 소스로 설치하려면 Rust 1.88 이상이 필요합니다.

```sh
git clone https://github.com/chaeyn/shardrop.git
cd shardrop
```

```sh
cargo install --path . --locked
shardrop doctor
```

미리 빌드한 실행 파일은 Rust 설치 없이 사용할 수 있습니다. 원본 서버와 받는 컴퓨터에 각각 해당 운영체제용 같은 버전을 설치합니다. SSH 전송에는 시스템 OpenSSH 클라이언트가 필요합니다. 압축·tar 처리·TLS는 실행 파일 안에서 수행하므로 Python, GNU tar, OpenSSL 명령은 필요하지 않습니다.

제공받은 릴리스 압축 파일은 `.sha256`과 비교한 뒤 풀고 실행 파일을 PATH에 넣습니다. Unix는 `scripts/install-local.sh ./shardrop`, Windows는 `scripts/install-local.ps1 -Binary .\shardrop.exe`로 사용자 디렉터리에 설치할 수 있습니다. 두 스크립트는 외부 코드를 내려받지 않습니다.

## 사용

```sh
# SSH로 전송
shardrop copy server /srv/project /srv/photos -o ./backup

# 내부망으로 데이터를 직접 전송하고, 연결 실패 시 SSH 터널 사용
shardrop copy server /srv/project -o ./backup --direct 192.168.1.20

# Ctrl+C로 수신을 멈춘 뒤 재개
shardrop resume ./backup

# 네트워크 없이 검증·복원
shardrop verify ./backup
shardrop restore ./backup --to ./empty-directory

# 로컬 폴더 압축
shardrop pack ./project -o ./project-backup
```

`--direct`에는 원본 서버 주소를 넣습니다. 이 옵션을 사용하면 원본 서버의 데이터 리스너를 IPv4 전체 인터페이스에 엽니다. 방화벽·라우터 설정은 바꾸지 않습니다. 고정 포트는 `--listen-port 9443`, 외부 포트 매핑은 `--direct-port`로 지정합니다. 직접 연결용 IPv6 리스너는 지원하지 않습니다. 작업 시작·재시작에는 SSH 접속이 필요합니다.

원격 실행 파일이 PATH에 없으면 `--remote-bin /실행파일/경로`를 지정합니다. Windows 원본에는 `--remote-shell powershell`과 Windows 절대 경로를 사용합니다. POSIX 원본의 `--sudo`는 기존 `sudo -n` 권한을 사용하며 비밀번호를 받거나 저장하지 않습니다.

```sh
shardrop copy server /srv/project -o ./backup \
  --chunk-mib 8 --compression-workers 4 --level 3 -j 8 \
  --exclude '**/node_modules/**' --exclude '**/.cache/**'
```

원격 원본 경로는 절대 경로여야 합니다. `pack`에는 상대 경로도 쓸 수 있습니다. `/srv/project`는 `project/`로 복원하므로 여러 원본의 마지막 디렉터리 이름은 서로 달라야 합니다. 제외 패턴은 원본 경로와 아카이브 경로에 적용합니다.

## 검증과 임시 파일

- 전송 완료한 조각마다 길이와 SHA-256을 검사합니다. 재개할 때도 기존 조각을 다시 검사합니다.
- 마지막에는 조각 순서, 매니페스트 해시, gzip CRC와 tar 구조를 검증합니다. 복원 대상은 빈 디렉터리여야 합니다.
- 원본 서버와 받는 컴퓨터에 각각 압축본 크기만큼 공간이 필요합니다. 기본 여유 공간 기준은 2 GiB이며 `--reserve-mib`로 조정합니다.
- 원본 임시 파일은 OS 임시 디렉터리에 둡니다. 최종 검증 성공 후 삭제를 요청합니다. `--keep-source`로 남기면 나중에 손상된 조각을 다시 받을 수 있습니다.
- `shardrop cleanup ./backup`은 검증 후 원본 임시 파일 삭제를 요청합니다. 미완료 작업을 폐기하려면 `--cancel`을 붙입니다. 실제 원본 경로는 삭제하지 않습니다.
- 원본 서버는 기본 24시간 뒤 중지합니다. `--ttl-seconds` 범위는 60초부터 7일입니다. 시간 만료 시 임시 파일은 남고, 압축을 완료한 작업은 `resume`으로 다시 열 수 있습니다.
- 압축 자체가 중단되었으면 새 백업을 시작해야 합니다. OS가 임시 파일을 지웠거나 정리가 끝난 뒤에는 누락된 조각을 복구할 수 없습니다.

심볼릭 링크의 대상은 따라가지 않습니다. 소켓·FIFO 등 특수 파일은 경고를 남기고 건너뜁니다. 읽는 중 바뀐 파일도 경고를 남기며, 크기가 줄어든 파일은 실패로 처리합니다. 실행 중인 데이터베이스처럼 일관성이 필요한 데이터는 스냅샷을 사용하거나 쓰기 작업을 멈춘 뒤 백업합니다.

파일 내용·디렉터리·수정 시각·기본 모드·심볼릭 링크와 Unix 하드링크를 다룹니다. ACL, xattr, 소유권 복원, Windows ADS, 희소 파일의 디스크 할당 형태는 보존하지 않습니다. 자세한 운영체제별 제한은 [호환 범위](docs/compatibility.md)를 확인하세요.

백업 폴더의 `session.json`에는 임시 작업 토큰이 있으므로 비공개로 보관해야 합니다. 전송 구간은 암호화하지만 저장한 조각은 암호화하지 않습니다. Unix에서는 디렉터리 권한 0700을 적용하며 Windows에서는 사용자 디렉터리 ACL에 의존합니다.

## 자동화·개발

`--json`은 최종 검증 결과를 stdout에 출력합니다. 진행 상황과 경고는 stderr에 출력합니다. 종료 코드는 성공 `0`, 작업 실패 `1`, 인자 오류 `2`, Ctrl+C 중단 `130`입니다. 경고가 있어도 성공 코드를 반환하므로 필요하면 `warning_count`를 검사합니다.

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
shardrop completions zsh
shardrop man
```

Linux·macOS·Windows CI와 태그 기반 릴리스 워크플로를 포함했습니다. 현재 실행 결과는 [Actions](https://github.com/chaeyn/shardrop/actions)에서 확인할 수 있습니다. [검증 기록](docs/validation.md)에 확인한 환경과 남은 항목을 구분했습니다.

MIT 라이선스. [uutils/coreutils](https://github.com/uutils/coreutils)의 크로스 플랫폼 CLI 배포 방식을 참고한 독립 프로젝트입니다.
