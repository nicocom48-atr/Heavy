use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "heavydrop",
    author = "HeavyDrop Contributors",
    version,
    about = "⚡ Transferencia de archivos P2P masivos (50GB+) mediante códigos temporales únicos sin configuración de IPs."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Enviar un archivo generando un código temporal de conexión
    Send {
        /// Modo anterior: código corto y descubrimiento TCP en la red local
        #[arg(long)]
        lan_only: bool,
        /// Ruta al archivo a transferir
        #[arg(value_name = "FILE")]
        file: PathBuf,

        /// Omitir el hash previo del archivo completo (los bloques se siguen verificando en tránsito)
        #[arg(long)]
        no_prehash: bool,

        /// Avanzado: Puerto TCP preferido para conexiones entrantes (por defecto: 7878)
        #[arg(long, default_value_t = 7878)]
        port: u16,

        /// Avanzado: Servidor de señalización personalizado (host:puerto)
        #[arg(long)]
        signal_server: Option<String>,
    },

    /// Recibir un archivo utilizando el código temporal compartido por el emisor
    Recv {
        /// Código de conexión temporal (ej. KD7M-9Q2P-W8RX)
        #[arg(value_name = "CODE")]
        code: String,

        /// Directorio donde se guardará el archivo recibido (por defecto: carpeta actual)
        #[arg(short, long, default_value = ".")]
        output_dir: PathBuf,

        /// Forzar sobrescritura: ignora descargas parciales existentes
        #[arg(short, long)]
        force: bool,

        /// Avanzado: Servidor de señalización personalizado (host:puerto)
        #[arg(long)]
        signal_server: Option<String>,
    },

    /// Ejecutar el servidor ligero de señalización y relay (rendezvous)
    Server {
        /// Puerto TCP para escuchar conexiones de señalización y relay (por defecto: 7880)
        #[arg(short, long, default_value_t = 7880)]
        port: u16,
    },

    /// Abrir la interfaz gráfica de escritorio (GUI)
    Gui,
}
