# ⚡ HeavyDrop: Transferencia P2P de Archivos Masivos (50GB+) en Rust

**HeavyDrop** es una herramienta en Rust diseñada específicamente para la transferencia punto a punto (P2P) de archivos extremadamente pesados (50 GB, 100 GB, 1 TB o más) de forma rápida, segura y resiliente.

Cuenta con una **interfaz gráfica nativa de escritorio (GUI)** moderna e intuitiva, además de una interfaz de línea de comandos (CLI) para terminal y automatizaciones.

---

## 🖥️ Interfaz Gráfica Nativa (GUI)

Al abrir el ejecutable haciendo doble clic en `heavydrop.exe` (o ejecutándolo sin argumentos), se abrirá la **ventana de escritorio**:

- 📁 **Selector de archivos nativo de Windows:** Elige el archivo de 50GB mediante la ventana del Explorador de archivos con un solo clic.
- 📋 **Detección y copia de IPs:** Muestra las direcciones IP de tu máquina (Wi-Fi, Ethernet, Tailscale/VPN) con botones de copia rápida para compartir con el otro equipo.
- 📊 **Progreso en tiempo real:** Barra animada con velocidad (MB/s), bytes transferidos, porcentaje y tiempo restante estimado (ETA).
- 🔄 **Reanudación visible:** Notificación visual clara si detecta un fragmento previo y continúa la descarga sin repetir datos.
- 🛑 **Botón de cancelación:** Permite pausar o cancelar la transferencia limpiamente en cualquier instante.

---

## 🚀 Características Técnicas

1. **Memoria RAM Mínima y Constante (< 20 MB):**
   - Transmisión en streaming por bloques (*chunks*, por defecto de 4 MB).
   - Nunca carga el archivo completo en memoria, permitiendo mover archivos de 50 GB o más sin riesgo de saturar la memoria RAM.

2. **Reanudación Automática Inteligente (*Resume*):**
   - Si la conexión se interrumpe a los 45 GB de 50 GB (por microcorte de red, apagado o Wi-Fi inestable), no pierdes los datos ya descargados.
   - El receptor detecta los bytes locales válidos, solicita la posición de reanudación y el emisor se posiciona (*seek*) instantáneamente en ese byte para continuar.

3. **Verificación de Integridad Criptográfica con BLAKE3:**
   - **En tránsito:** Cada chunk de 4 MB se verifica con un hash BLAKE3 antes de guardarse en el disco de destino. Si un paquete se corrompe en la red, se detecta de inmediato.
   - **Fin de transferencia:** Se comprueba el hash global del archivo contra el anunciado por el emisor.
   - **Rendimiento:** BLAKE3 es hasta 10 veces más rápido que SHA-256 (procesa 3 a 5 GB/s en procesadores modernos gracias a SIMD y Rayon), lo que evita cuellos de botella en la CPU.

4. **Monitoreo en Tiempo Real:**
   - Barra de progreso detallada con velocidad de transferencia en tiempo real (MB/s), bytes transferidos, porcentaje y tiempo restante estimado (ETA).

5. **Alineación y Cero Sobrecarga de Protocolo:**
   - Encabezado binario ligero con *magic bytes* (`HVDROP01`), negociación JSON tipada y *TCP_NODELAY* activado para minimizar la latencia.

6. **Seguridad Opcional con Token:**
   - Posibilidad de proteger la transferencia con una clave secreta (`--token`), asegurando que solo el receptor autorizado pueda descargar el archivo.

---

## 📁 Estructura del Código

```text
heavydrop/
├── Cargo.toml            # Dependencias: tokio, blake3, indicatif, clap, serde, etc.
├── README.md             # Documentación detallada
└── src/
    ├── main.rs           # Punto de entrada y gestión de errores
    ├── cli.rs            # Definición de argumentos de línea de comandos con clap
    ├── protocol.rs       # Definición de tramas, handshake, chunks binarios y magic bytes
    ├── integrity.rs      # Hashing ultra-rápido multihilo con BLAKE3 y barra de progreso
    ├── network.rs        # Detección de interfaces LAN (Wi-Fi, Ethernet, Tailscale) y formateo de IPs
    ├── sender.rs         # Servidor emisor con soporte para seek/resume y streaming de chunks
    └── receiver.rs       # Cliente receptor con verificación de hashes en tiempo real y persistencia
```

---

## 🛠️ Requisitos Previos e Instalación

Para compilar y ejecutar el proyecto necesitas tener instalado el compilador de **Rust**:

### 1. Instalar Rust (si aún no lo tienes)

- **Windows:** Descarga e instala `rustup-init.exe` desde [rustup.rs](https://rustup.rs/) o ejecútalo mediante PowerShell:
  ```powershell
  winget install Rustlang.Rustup
  ```
  *(Luego reinicia la terminal para que cargue la variable de entorno `cargo`).*

- **Linux / macOS:**
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```

### 2. Compilar en modo Release (Máximo Rendimiento)

Abre la terminal en la carpeta del proyecto `heavydrop`:

```bash
cargo build --release
```

El ejecutable optimizado estará disponible en:
- **Windows:** `target/release/heavydrop.exe`
- **Linux/macOS:** `target/release/heavydrop`

---

## 📖 Guía de Uso

### 1. Modo Envío (`send`)

Para enviar un archivo pesado (ej. un ISO de 50 GB):

```bash
heavydrop send "F:\backups\archivo_50gb.iso"
```

El programa calculará el hash BLAKE3 y mostrará automáticamente las opciones de conexión para compartir con el otro equipo:

```text
================ [ HEAVYDROP P2P TRANSFER ] ================
Waiting for receiver peer to connect...
Share one of the following commands with the receiver:

  ▶ [Wi-Fi LAN] (wlan0): heavydrop recv 192.168.1.45:7878
  ▶ [Tailscale / VPN] (tailscale0): heavydrop recv 100.85.20.12:7878
============================================================
```

#### Opciones avanzadas de envío:

- **Proteger con contraseña / token:**
  ```bash
  heavydrop send "archivo.iso" --token mi_clave_secreta_123
  ```

- **Saltar el pre-cálculo del hash para inicio instantáneo:**
  *(Útil si el archivo está en un disco rígido mecánico y no quieres esperar antes de enviar; cada chunk de 4MB se seguirá verificando en vuelo)*:
  ```bash
  heavydrop send "archivo.iso" --no-prehash
  ```

- **Cambiar el tamaño de fragmento (*chunk size*):**
  ```bash
  heavydrop send "archivo.iso" --chunk-size-mb 8 --port 9000
  ```

---

### 2. Modo Recepción (`recv`)

En el equipo que recibirá el archivo, ejecuta el comando indicado por el emisor:

```bash
heavydrop recv 192.168.1.45:7878
```

O especificando una carpeta de destino:

```bash
heavydrop recv 192.168.1.45:7878 --output-dir "D:\Descargas"
```

Si el emisor configuró un token de seguridad:
```bash
heavydrop recv 192.168.1.45:7878 --token mi_clave_secreta_123
```

---

## 🔄 Reanudación de Descargas Interrumpidas (*Resume*)

Si por algún motivo la transferencia se interrumpe a la mitad (por ejemplo, a los 35 GB):

1. **Vuelve a lanzar el emisor:**
   ```bash
   heavydrop send "archivo_50gb.iso"
   ```
2. **Vuelve a lanzar el receptor:**
   ```bash
   heavydrop recv 192.168.1.45:7878
   ```

El receptor identificará que ya existen 35 GB descargados en el disco de destino, calculará el último fragmento íntegro y le solicitará al emisor empezar exactamente desde ese byte. **No volverás a transferir los primeros 35 GB.**

---

## 🧪 Prueba Rápida con un Archivo Simulado de 50 GB

En Windows puedes crear un archivo de prueba de 50 GB en 1 segundo usando `fsutil`:

```powershell
# Crea un archivo vacío de 50 GB al instante (53.687.091.200 bytes)
fsutil file createnew archivo_50gb_prueba.dat 53687091200
```

Luego, en una terminal:
```powershell
cargo run --release -- send archivo_50gb_prueba.dat
```

Y en otra terminal (en la misma PC o en otra máquina en red):
```powershell
cargo run --release -- recv 127.0.0.1:7878 --output-dir ./descargas
```

Podrás ver la barra de progreso a gigabits por segundo, probar cancelarlo con `Ctrl+C` a la mitad y volver a ejecutar el comando para ver cómo reanuda (*resume*) sin perder los datos previos.

---

## 🌐 Conexión a Través de Internet (WAN / NAT Traversal)

Para transferir archivos P2P entre dos computadoras que no están en la misma red local (LAN) y están detrás de routers con NAT:

1. **Opción Recomendada: VPN Mesh P2P (Tailscale o ZeroTier)**
   - Instala [Tailscale](https://tailscale.com/) en ambas computadoras.
   - HeavyDrop detectará la IP de Tailscale (ejemplo: `100.x.y.z`) y la conexión P2P se establecerá directamente y cifrada sin necesidad de abrir puertos en el router.

2. **Opción Clásica: Reenvío de Puertos (*Port Forwarding*)**
   - En el router del emisor, abre y redirige el puerto TCP `7878` hacia la IP local de la computadora emisora.
   - El receptor simplemente se conecta a la IP pública del emisor: `heavydrop recv TU_IP_PUBLICA:7878`.
