use quick_xml::events::{attributes::Attributes, BytesStart, Event};
use std::io::{BufRead, BufReader, Result};
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, ChildStdout, Command, Stdio};
use typed_builder::TypedBuilder;

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum InputType {
    File,
    Fifo,
}

pub enum Output {
    Packet(Packet),
    EOF,
    Empty,
}

#[derive(Clone, Default)]
pub struct Metadata {
    name: String,
    value: String,
    display: String,
    size: u32,
    position: u32,
}

impl Metadata {
    pub fn new(name: String, value: String) -> Metadata {
        Metadata {
            name,
            value,
            ..Default::default()
        }
    }
    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    pub fn value(&self) -> &str {
        self.value.as_str()
    }

    pub fn display(&self) -> &str {
        self.display.as_str()
    }

    pub fn size(&self) -> u32 {
        self.size
    }

    pub fn position(&self) -> u32 {
        self.position
    }
}

impl IntoIterator for Layer {
    type Item = Metadata;
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.metadata.into_iter()
    }
}
#[derive(Clone)]
pub struct Layer {
    name: String,
    index: usize,
    metadata: Vec<Metadata>,
}

impl Layer {
    pub fn new(name: String, index: usize) -> Self {
        Layer {
            name,
            index,
            metadata: vec![],
        }
    }

    pub fn add(&mut self, metadata: Metadata) {
        self.metadata.push(metadata);
    }

    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn iter(&self) -> impl Iterator<Item = &Metadata> {
        self.metadata.iter()
    }

    pub fn metadata(&self, name: &str) -> Option<&Metadata> {
        self.metadata.iter().find(|m| m.name().eq(name))
    }
}

pub struct Packet {
    layers: Vec<Layer>,
}

impl Packet {
    fn new() -> Self {
        Packet { layers: vec![] }
    }

    fn push(&mut self, name: String) {
        let layer = Layer::new(name, self.layers.len());
        self.layers.push(layer);
    }

    fn last_layer_mut(&mut self) -> Option<&mut Layer> {
        self.layers.last_mut()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Layer> {
        self.layers.iter()
    }

    pub fn layer_index(&self, index: usize) -> Option<&Layer> {
        self.layers.get(index)
    }

    pub fn layer_name(&self, name: &str) -> Option<&Layer> {
        self.layers.iter().find(|&layer| layer.name.eq(name))
    }

    pub fn layer_name_mut(&mut self, name: &str) -> Option<&mut Layer> {
        self.layers.iter_mut().find(|layer| layer.name.eq(name))
    }
}

impl IntoIterator for Packet {
    type Item = Layer;
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.layers.into_iter()
    }
}

#[derive(TypedBuilder)]
pub struct RTSharkBuilder<'a> {
    input_path: String,
    input_type: InputType,
    #[builder(default)]
    metadata_blacklist: Vec<&'a str>,
    #[builder(default)]
    env_path: &'a str,
}

impl<'a> RTSharkBuilder<'a> {
    pub fn run(&self) -> Result<RTShark> {
        // test if input file exists
        std::fs::metadata(&self.input_path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => std::io::Error::new(
                e.kind(),
                format!("Unable to find {}: {}", &self.input_path, e),
            ),
            _ => e,
        })?;

        // prepare tshark command line parameters
        let mut tshark_params = vec![
            if self.input_type == InputType::File {
                "-r"
            } else {
                "-i"
            },
            self.input_path.as_str(),
            "-Tpdml",
            "-n",
        ];

        // it would be possible to ask tshark to "mix in" a keylog file
        // when opening the pcap file
        // (obtain the keylog file through `SSLKEYLOGFILE=browser_keylog.txt google-chrome` or firefox,
        // pass it to tshark through "-o ssh.keylog_file:/path/to/keylog")
        // but we get in flatpak limitations (can only access the file that the user opened
        // due to the sandbox) => better to just mix in the secrets manually and open a single
        // file. this is done through => editcap --inject-secrets tls,/path/to/keylog.txt ~/testtls.pcap ~/outtls.pcapng

        if self.input_type == InputType::Fifo {
            tshark_params.extend(&["-l"]);
        }

        /* TODO : implement filters
        {
            //tshark_params.extend(&[filters]);
        }
        */

        // piping from tshark, not to load the entire JSON in ram...
        // this may fail if tshark is not found in path

        let tshark_child = if self.env_path.is_empty() {
            Command::new("tshark")
                .args(&tshark_params)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
        } else {
            Command::new("tshark")
                .args(&tshark_params)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .env("PATH", self.env_path)
                .spawn()
        };

        let mut tshark_child = tshark_child.map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                std::io::Error::new(e.kind(), format!("Unable to find tshark: {}", e))
            }
            _ => e,
        })?;

        let buf_reader = BufReader::new(tshark_child.stdout.take().unwrap());

        let reader = quick_xml::Reader::from_reader(buf_reader);

        let filters: Vec<String> = self
            .metadata_blacklist
            .iter()
            .map(|s| s.to_string())
            .collect();
        Ok(RTShark::new(tshark_child, reader, filters))
    }
}

pub struct RTShark {
    process: Option<Child>,
    parser: quick_xml::Reader<BufReader<ChildStdout>>,
    filters: Vec<String>,
}

impl RTShark {
    fn new(
        process: Child,
        parser: quick_xml::Reader<BufReader<ChildStdout>>,
        filters: Vec<String>,
    ) -> Self {
        RTShark {
            process: Some(process),
            parser,
            filters,
        }
    }

    fn attr_by_name<'a>(attrs: &mut Attributes<'a>, key: &[u8]) -> Result<String> {
        for attr in attrs {
            let attr = attr.map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Error decoding xml attribute: {e:?}"),
                )
            })?;
            if attr.key == key {
                let value = std::str::from_utf8(&attr.value).map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Error decoding utf8 value: {e:?}"),
                    )
                })?;
                return Ok(value.to_owned());
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "xml lookup error: no key '{}'",
                std::str::from_utf8(key).unwrap()
            ),
        ))
    }

    fn attr_by_name_u32<'a>(attrs: &mut Attributes<'a>, key: &[u8]) -> Result<u32> {
        match RTShark::attr_by_name(attrs, key) {
            Err(e) => Err(e),
            Ok(v) => v.parse::<u32>().map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Error decoding u32 value: {e:?}"),
                )
            }),
        }
    }

    fn build_metadata(tag: &BytesStart, filters: &[String]) -> Result<Option<Metadata>> {
        let name = RTShark::attr_by_name(&mut tag.attributes(), b"name")?;
        // skip data
        if filters.contains(&name) {
            return Ok(None);
        }

        let value = RTShark::attr_by_name(&mut tag.attributes(), b"show")?;

        let mut metadata = Metadata::new(name, value);

        if let Ok(position) = RTShark::attr_by_name_u32(&mut tag.attributes(), b"pos") {
            metadata.position = position;
        }
        if let Ok(size) = RTShark::attr_by_name_u32(&mut tag.attributes(), b"size") {
            metadata.size = size;
        }
        if let Ok(display) = RTShark::attr_by_name(&mut tag.attributes(), b"showname") {
            metadata.display = display;
        }
        Ok(Some(metadata))
    }

    fn parse_xml<B: BufRead>(
        xml_reader: &mut quick_xml::Reader<B>,
        filters: &[String],
    ) -> Result<Output> {
        let mut buf = vec![];
        let mut packet = Packet::new();
        let mut store_metadata = false;

        loop {
            match xml_reader.read_event(&mut buf) {
                Ok(Event::Start(ref e)) => match e.name() {
                    b"packet" => (),
                    b"proto" => {
                        let proto = RTShark::attr_by_name(&mut e.attributes(), b"name")?;

                        if proto.eq("fake-field-wrapper") {
                            store_metadata = false;
                            continue;
                        } else {
                            store_metadata = true;
                        }

                        packet.push(proto);
                    }
                    b"field" => {
                        if !store_metadata {
                            continue;
                        }

                        let metadata = RTShark::build_metadata(e, filters)?;
                        if let Some(metadata) = metadata {
                            packet.last_layer_mut().unwrap().add(metadata);
                        }
                    }
                    _ => (),
                },
                Ok(Event::Empty(ref e)) => match e.name() {
                    b"packet" => (),
                    b"proto" => (),
                    b"field" => {
                        if !store_metadata {
                            continue;
                        }

                        let metadata = RTShark::build_metadata(e, filters)?;
                        if let Some(metadata) = metadata {
                            packet.last_layer_mut().unwrap().add(metadata);
                        }
                    }
                    _ => (),
                },
                Ok(Event::End(ref e)) => match e.name() {
                    b"packet" => return Ok(Output::Packet(packet)),
                    b"proto" => (),
                    b"field" => (),
                    _ => (),
                },

                Ok(Event::Eof) => {
                    return Ok(Output::EOF);
                }
                Err(e) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "xml parsing error: {} at tshark output offset {}",
                            e,
                            xml_reader.buffer_position()
                        ),
                    ));
                }
                Ok(_) => (),
            }
        }
    }

    pub fn read(&mut self) -> Result<Output> {
        let xml_reader = &mut self.parser;

        let msg = RTShark::parse_xml(xml_reader, &self.filters);
        if let Ok(ref msg) = msg {
            let done = match msg {
                Output::EOF => match self.process {
                    Some(ref mut process) => RTShark::try_wait_has_exited(process),
                    _ => true,
                },
                _ => false,
            };

            if done {
                self.process = None;
            }
        }

        msg
    }

    pub fn kill(&mut self) {
        if let Some(ref mut process) = self.process {
            let done = match process.try_wait() {
                Ok(maybe) => match maybe {
                    None => false,
                    Some(_exitcode) => true,
                },
                Err(e) => {
                    eprintln!("Error while killing rtshark: wait: {e}");
                    false
                }
            };

            if !done {
                match process.kill() {
                    Ok(()) => (),
                    Err(e) => eprintln!("Error while killing rtshark: kill: {e}"),
                }
                if let Err(e) = process.wait() {
                    eprintln!("Error while killing rtshark: wait: {e}");
                }
            }

            self.process = None;
        }
    }

    pub fn pid(&self) -> Option<u32> {
        self.process.as_ref().map(|p| p.id())
    }

    pub fn try_wait_has_exited(child: &mut Child) -> bool {
        matches!(child.try_wait(), Ok(Some(s)) if s.code().is_some() || s.signal().is_some())
    }
}

impl Drop for RTShark {
    fn drop(&mut self) {
        self.kill()
    }
}

#[cfg(test)]
mod tests {

    use std::io::Write;

    use super::*;

    #[test]
    fn test_parse_single_proto_metadata() {
        let xml = r#"
        <pdml>
         <packet>
          <proto name="frame">
           <field name="frame.time" show="test time" pos="0" size="0" showname="test time display"/>
          </proto>
         </packet>
        </pdml>"#;

        let mut reader = quick_xml::Reader::from_reader(BufReader::new(xml.as_bytes()));

        let msg = RTShark::parse_xml(&mut reader, &vec![]).unwrap();
        let pkt = match msg {
            Output::Packet(p) => p,
            _ => panic!("invalid Output type"),
        };

        assert_eq!(pkt.layers.len(), 1);
        for layer in pkt.layers {
            for m in layer {
                assert!(m.name().eq("frame.time"));
                assert!(m.value().eq("test time"));
                assert!(m.display().eq("test time display"));
            }
        }
    }

    #[test]
    fn test_parse_missing_optional_size() {
        let xml = r#"
        <pdml>
         <packet>
          <proto name="frame">
          <field name="frame.time" show="test time" pos="0" showname="test time display"/>
          </proto>
         </packet>
        </pdml>"#;

        let mut reader = quick_xml::Reader::from_reader(BufReader::new(xml.as_bytes()));

        let msg = RTShark::parse_xml(&mut reader, &vec![]).unwrap();
        let pkt = match msg {
            Output::Packet(p) => p,
            _ => panic!("invalid Output type"),
        };

        assert_eq!(pkt.layers.len(), 1);
    }

    #[test]
    fn test_parse_missing_optional_pos() {
        let xml = r#"
        <pdml>
         <packet>
          <proto name="frame">
          <field name="frame.time" show="test time" size="0" showname="test time display"/>
          </proto>
         </packet>
        </pdml>"#;

        let mut reader = quick_xml::Reader::from_reader(BufReader::new(xml.as_bytes()));

        let msg = RTShark::parse_xml(&mut reader, &vec![]).unwrap();
        let pkt = match msg {
            Output::Packet(p) => p,
            _ => panic!("invalid Output type"),
        };

        assert_eq!(pkt.layers.len(), 1);
    }

    #[test]
    fn test_parse_missing_optional_display() {
        let xml = r#"
        <pdml>
         <packet>
          <proto name="frame">
          <field name="frame.time" show="test time" pos="0" size="0" />
          </proto>
         </packet>
        </pdml>"#;

        let mut reader = quick_xml::Reader::from_reader(BufReader::new(xml.as_bytes()));

        let msg = RTShark::parse_xml(&mut reader, &vec![]).unwrap();
        let pkt = match msg {
            Output::Packet(p) => p,
            _ => panic!("invalid Output type"),
        };

        assert_eq!(pkt.layers.len(), 1);
    }

    #[test]
    fn test_parse_missing_mandatory_name() {
        let xml = r#"
        <pdml>
         <packet>
          <proto name="frame">
          <field show="test time" pos="0" size="0" showname="test time display"/>
          </proto>
         </packet>
        </pdml>"#;

        let mut reader = quick_xml::Reader::from_reader(BufReader::new(xml.as_bytes()));

        let msg = RTShark::parse_xml(&mut reader, &vec![]);

        match msg {
            Err(_) => (),
            _ => panic!("invalid result"),
        }
    }

    #[test]
    fn test_parse_missing_mandatory_show() {
        let xml = r#"
        <pdml>
         <packet>
          <proto name="frame">
          <field name="frame.time" pos="0" size="0" showname="test time display"/>
          </proto>
         </packet>
        </pdml>"#;

        let mut reader = quick_xml::Reader::from_reader(BufReader::new(xml.as_bytes()));

        let msg = RTShark::parse_xml(&mut reader, &vec![]);
        match msg {
            Err(_) => (),
            _ => panic!("invalid result"),
        }
    }

    const XML_TCP: &'static str = r#"
    <pdml>
     <packet>
      <proto name="frame">
       <field name="frame.time" show="Mar  5, 2021 08:49:52.736275000 CET"/>
      </proto>
      <proto name="ip">
       <field name="ip.src" show="1.1.1.1" />
       <field name="ip.dst" show="1.1.1.2" />
      </proto>
      <proto name="tcp">
       <field name="tcp.srcport" show="52796" value="ce3c"/>
       <field name="tcp.dstport" show="5432" value="1538"/>
       <field name="tcp.seq_raw" show="1963007432" value="75011dc8"/>
       <field name="tcp.stream" show="4"/>
      </proto>
     </packet>
    </pdml>"#;

    #[test]
    fn test_access_packet_into_iter() {
        let mut reader = quick_xml::Reader::from_reader(BufReader::new(XML_TCP.as_bytes()));

        let msg = RTShark::parse_xml(&mut reader, &vec![]).unwrap();
        let pkt = match msg {
            Output::Packet(p) => p,
            _ => panic!("invalid Output type"),
        };

        assert_eq!(pkt.layers.len(), 3);

        let mut iter = pkt.into_iter();
        let frame = iter.next().unwrap();
        assert!(frame.name().eq("frame"));
        let ip = iter.next().unwrap();
        assert!(ip.name().eq("ip"));
        let tcp = iter.next().unwrap();
        assert!(tcp.name().eq("tcp"));
        assert!(iter.next().is_none());
    }

    #[test]
    fn test_access_packet_iter() {
        let mut reader = quick_xml::Reader::from_reader(BufReader::new(XML_TCP.as_bytes()));

        let msg = RTShark::parse_xml(&mut reader, &vec![]).unwrap();
        let pkt = match msg {
            Output::Packet(p) => p,
            _ => panic!("invalid Output type"),
        };

        assert_eq!(pkt.layers.len(), 3);

        let mut iter = pkt.iter();
        let frame = iter.next().unwrap();
        assert!(frame.name().eq("frame"));
        let ip = iter.next().unwrap();
        assert!(ip.name().eq("ip"));
        let tcp = iter.next().unwrap();
        assert!(tcp.name().eq("tcp"));
        assert!(iter.next().is_none());
    }

    #[test]
    fn test_access_layer_index() {
        let mut reader = quick_xml::Reader::from_reader(BufReader::new(XML_TCP.as_bytes()));

        let msg = RTShark::parse_xml(&mut reader, &vec![]).unwrap();
        let pkt = match msg {
            Output::Packet(p) => p,
            _ => panic!("invalid Output type"),
        };

        assert_eq!(pkt.layers.len(), 3);

        assert!(pkt.layer_index(0).unwrap().name().eq("frame"));
        assert!(pkt.layer_index(1).unwrap().name().eq("ip"));
        assert!(pkt.layer_index(2).unwrap().name().eq("tcp"));

        assert!(pkt.layer_index(3).is_none());
    }

    #[test]
    fn test_access_layer_name() {
        let mut reader = quick_xml::Reader::from_reader(BufReader::new(XML_TCP.as_bytes()));

        let msg = RTShark::parse_xml(&mut reader, &vec![]).unwrap();
        let pkt = match msg {
            Output::Packet(p) => p,
            _ => panic!("invalid Output type"),
        };

        assert_eq!(pkt.layers.len(), 3);

        assert!(pkt.layer_name("frame").unwrap().name().eq("frame"));
        assert!(pkt.layer_name("ip").unwrap().name().eq("ip"));
        assert!(pkt.layer_name("tcp").unwrap().name().eq("tcp"));

        assert!(pkt.layer_name("udp").is_none());
    }

    #[test]
    fn test_access_layer_name_with_tunnel() {
        let xml = r#"
        <pdml>
         <packet>
          <proto name="frame">
           <field name="frame.time" show="Mar  5, 2021 08:49:52.736275000 CET"/>
          </proto>
          <proto name="ip">
           <field name="ip.src" show="10.215.215.9" />
           <field name="ip.dst" show="10.215.215.10" />
          </proto>
          <proto name="ip">
           <field name="ip.src" show="10.10.215.9" />
           <field name="ip.dst" show="10.10.215.10" />
          </proto>
          <proto name="tcp">
           <field name="tcp.srcport" show="52796" value="ce3c"/>
           <field name="tcp.dstport" show="5432" value="1538"/>
           <field name="tcp.seq_raw" show="1963007432" value="75011dc8"/>
           <field name="tcp.stream" show="4"/>
          </proto>
         </packet>
        </pdml>"#;

        let mut reader = quick_xml::Reader::from_reader(BufReader::new(xml.as_bytes()));

        let msg = RTShark::parse_xml(&mut reader, &vec![]).unwrap();
        let pkt = match msg {
            Output::Packet(p) => p,
            _ => panic!("invalid Output type"),
        };

        assert_eq!(pkt.layers.len(), 4);

        assert!(pkt.layer_name("frame").unwrap().name().eq("frame"));
        assert!(pkt.layer_name("ip").unwrap().name().eq("ip"));
        assert!(pkt.layer_name("ip").unwrap().index() == 1usize);
        assert!(pkt.layer_index(1).unwrap().name().eq("ip"));
        assert!(pkt.layer_index(2).unwrap().name().eq("ip"));
        assert!(pkt.layer_name("tcp").unwrap().name().eq("tcp"));

        assert!(pkt.layer_name("udp").is_none());
    }

    #[test]
    fn test_access_layer_iter() {
        let mut reader = quick_xml::Reader::from_reader(BufReader::new(XML_TCP.as_bytes()));

        let msg = RTShark::parse_xml(&mut reader, &vec![]).unwrap();
        let pkt = match msg {
            Output::Packet(p) => p,
            _ => panic!("invalid Output type"),
        };

        let ip = pkt.layer_name("ip").unwrap();
        let mut iter = ip.iter();
        assert!(iter.next().unwrap().name().eq("ip.src"));
        assert!(iter.next().unwrap().name().eq("ip.dst"));
        assert!(iter.next().is_none());
    }

    #[test]
    fn test_access_layer_into_iter() {
        let mut reader = quick_xml::Reader::from_reader(BufReader::new(XML_TCP.as_bytes()));

        let msg = RTShark::parse_xml(&mut reader, &vec![]).unwrap();
        let pkt = match msg {
            Output::Packet(p) => p,
            _ => panic!("invalid Output type"),
        };

        let ip = pkt.layer_name("ip").unwrap().clone();
        let mut iter = ip.into_iter();
        assert!(iter.next().unwrap().name().eq("ip.src"));
        assert!(iter.next().unwrap().name().eq("ip.dst"));
        assert!(iter.next().is_none());
    }

    #[test]
    fn test_access_layer_metadata() {
        let mut reader = quick_xml::Reader::from_reader(BufReader::new(XML_TCP.as_bytes()));

        let msg = RTShark::parse_xml(&mut reader, &vec![]).unwrap();
        let pkt = match msg {
            Output::Packet(p) => p,
            _ => panic!("invalid Output type"),
        };

        let ip = pkt.layer_name("ip").unwrap();
        let src = ip.metadata("ip.src").unwrap();
        assert!(src.value().eq("1.1.1.1"));

        let dst = ip.metadata("ip.dst").unwrap();
        assert!(dst.value().eq("1.1.1.2"));
    }

    #[test]
    fn test_parser_filter_metadata() {
        let mut reader = quick_xml::Reader::from_reader(BufReader::new(XML_TCP.as_bytes()));

        let msg = RTShark::parse_xml(&mut reader, &vec!["ip.src".to_string()]).unwrap();
        let pkt = match msg {
            Output::Packet(p) => p,
            _ => panic!("invalid Output type"),
        };

        let ip = pkt.layer_name("ip").unwrap();
        assert!(ip.metadata("ip.src").is_none());
        assert!(ip.metadata("ip.dst").unwrap().value().eq("1.1.1.2"));
    }

    #[test]
    fn test_parser_multiple_packets() {
        let xml = r#"
        <pdml>
         <packet>
          <proto name="tcp"></proto>
         </packet>
         <packet>
          <proto name="udp"></proto>
         </packet>
         <packet>
          <proto name="igmp"></proto>
         </packet>
        </pdml>"#;

        let mut reader = quick_xml::Reader::from_reader(BufReader::new(xml.as_bytes()));
        match RTShark::parse_xml(&mut reader, &vec![]).unwrap() {
            Output::Packet(p) => assert!(p.layer_name("tcp").is_some()),
            _ => panic!("invalid Output type"),
        }
        match RTShark::parse_xml(&mut reader, &vec![]).unwrap() {
            Output::Packet(p) => assert!(p.layer_name("udp").is_some()),
            _ => panic!("invalid Output type"),
        }
        match RTShark::parse_xml(&mut reader, &vec![]).unwrap() {
            Output::Packet(p) => assert!(p.layer_name("igmp").is_some()),
            _ => panic!("invalid Output type"),
        }
        match RTShark::parse_xml(&mut reader, &vec![]).unwrap() {
            Output::EOF => (),
            _ => panic!("invalid Output type"),
        }
    }

    #[test]
    fn test_rtshark_input_pcap() {
        let pcap = include_bytes!("test.pcap");

        // create temp dir and copy pcap in it
        let tmp_dir = tempdir::TempDir::new("test_fifo").unwrap();
        let fifo_path = tmp_dir.path().join("file.pcap");
        let mut output = std::fs::File::create(&fifo_path).expect("unable to open file");
        output.write_all(pcap).expect("unable to write pcap");
        output.flush().expect("unable to flush");

        // run tshark on it
        let builder = RTSharkBuilder::builder()
            .input_path(fifo_path.to_str().unwrap().to_string())
            .input_type(InputType::File)
            .build();
        let mut rtshark = builder.run().unwrap();

        // read a packet
        match rtshark.read().unwrap() {
            Output::Packet(p) => assert!(p.layer_name("udp").is_some()),
            _ => panic!("invalid Output type"),
        }

        loop {
            match rtshark.read().unwrap() {
                Output::EOF => break,
                _ => (),
            }
        }

        rtshark.kill();

        assert!(rtshark.pid().is_none());

        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[test]
    fn test_rtshark_input_pcap_blacklist() {
        let pcap = include_bytes!("test.pcap");

        // create temp dir and copy pcap in it
        let tmp_dir = tempdir::TempDir::new("test_fifo").unwrap();
        let fifo_path = tmp_dir.path().join("file.pcap");
        let mut output = std::fs::File::create(&fifo_path).expect("unable to open file");
        output.write_all(pcap).expect("unable to write pcap");
        output.flush().expect("unable to flush");

        // run tshark on it
        let builder = RTSharkBuilder::builder()
            .input_path(fifo_path.to_str().unwrap().to_string())
            .input_type(InputType::File)
            .metadata_blacklist(vec!["ip.src"])
            .build();
        let mut rtshark = builder.run().unwrap();

        // read a packet
        let pkt = match rtshark.read().unwrap() {
            Output::Packet(p) => p,
            _ => panic!("invalid Output type"),
        };

        let ip = pkt.layer_name("ip").unwrap();
        assert!(ip.metadata("ip.src").is_none());
        assert!(ip.metadata("ip.dst").unwrap().value().eq("127.0.0.1"));

        rtshark.kill();

        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[test]
    fn test_rtshark_input_fifo() {
        let pcap = include_bytes!("test.pcap");

        // create temp dir
        let tmp_dir = tempdir::TempDir::new("test_fifo").unwrap();
        let fifo_path = tmp_dir.path().join("pcap.pipe");

        // create new fifo and give read, write and execute rights to the owner
        nix::unistd::mkfifo(&fifo_path, nix::sys::stat::Mode::S_IRWXU)
            .expect("Error creating fifo");

        // start tshark on the fifo
        let builder = RTSharkBuilder::builder()
            .input_path(fifo_path.to_str().unwrap().to_string())
            .input_type(InputType::Fifo)
            .build();
        let mut rtshark = builder.run().unwrap();

        // send packets in the fifo
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .open(&fifo_path)
            .expect("unable to open fifo");
        output.write_all(pcap).expect("unable to write in fifo");

        // get analysis
        match rtshark.read().unwrap() {
            Output::Packet(p) => assert!(p.layer_name("udp").is_some()),
            _ => panic!("invalid Output type"),
        }

        // stop tshark
        rtshark.kill();

        // verify tshark is stopped
        assert!(rtshark.pid().is_none());

        /* remove fifo & tempdir */
        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[test]
    fn test_rtshark_drop() {
        // create temp dir
        let tmp_dir = tempdir::TempDir::new("test_fifo").unwrap();
        let fifo_path = tmp_dir.path().join("pcap.pipe");

        // create new fifo and give read, write and execute rights to the owner
        nix::unistd::mkfifo(&fifo_path, nix::sys::stat::Mode::S_IRWXU)
            .expect("Error creating fifo");

        // start tshark on the fifo
        let builder = RTSharkBuilder::builder()
            .input_path(fifo_path.to_str().unwrap().to_string())
            .input_type(InputType::Fifo)
            .build();

        let pid = {
            let rtshark = builder.run().unwrap();
            let pid = rtshark.pid().unwrap();

            assert!(std::path::Path::new(&format!("/proc/{pid}")).exists());
            pid
        };

        // verify tshark is stopped
        assert!(std::path::Path::new(&format!("/proc/{pid}")).exists() == false);

        /* remove fifo & tempdir */
        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[test]
    fn test_rtshark_killed() {
        // create temp dir
        let tmp_dir = tempdir::TempDir::new("test_fifo").unwrap();
        let fifo_path = tmp_dir.path().join("pcap.pipe");

        // create new fifo and give read, write and execute rights to the owner
        nix::unistd::mkfifo(&fifo_path, nix::sys::stat::Mode::S_IRWXU)
            .expect("Error creating fifo");

        // start tshark on the fifo
        let builder = RTSharkBuilder::builder()
            .input_path(fifo_path.to_str().unwrap().to_string())
            .input_type(InputType::Fifo)
            .build();

        let mut rtshark = builder.run().unwrap();

        // killing badly
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(rtshark.pid().unwrap() as libc::pid_t),
            nix::sys::signal::Signal::SIGKILL,
        )
        .unwrap();

        // reading from process output should give EOF
        match rtshark.read().unwrap() {
            Output::EOF => (),
            _ => panic!("invalid Output type"),
        }

        /* remove fifo & tempdir */
        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[test]
    fn test_rtshark_fifo_lost() {
        // create temp dir
        let tmp_dir = tempdir::TempDir::new("test_fifo").unwrap();
        let fifo_path = tmp_dir.path().join("pcap.pipe");

        // create new fifo and give read, write and execute rights to the owner
        nix::unistd::mkfifo(&fifo_path, nix::sys::stat::Mode::S_IRWXU)
            .expect("Error creating fifo");

        // start tshark on the fifo
        let builder = RTSharkBuilder::builder()
            .input_path(fifo_path.to_str().unwrap().to_string())
            .input_type(InputType::Fifo)
            .build();

        let mut rtshark = builder.run().unwrap();

        /* remove fifo & tempdir */
        tmp_dir.close().expect("Error deleting fifo dir");

        // reading from process output should give EOF
        match rtshark.read().unwrap() {
            Output::EOF => (),
            _ => panic!("invalid Output type"),
        }
    }

    #[test]
    fn test_rtshark_fifo_opened_then_closed() {
        let pcap = include_bytes!("test.pcap");

        // create temp dir
        let tmp_dir = tempdir::TempDir::new("test_fifo").unwrap();
        let fifo_path = tmp_dir.path().join("pcap.pipe");

        // create new fifo and give read, write and execute rights to the owner
        nix::unistd::mkfifo(&fifo_path, nix::sys::stat::Mode::S_IRWXU)
            .expect("Error creating fifo");

        // start tshark on the fifo
        let builder = RTSharkBuilder::builder()
            .input_path(fifo_path.to_str().unwrap().to_string())
            .input_type(InputType::Fifo)
            .build();

        let mut rtshark = builder.run().unwrap();

        // send packets in the fifo then close it immediately
        {
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .open(&fifo_path)
                .expect("unable to open fifo");
            output.write_all(pcap).expect("unable to write in fifo");
        }

        // get analysis
        match rtshark.read().unwrap() {
            Output::Packet(p) => assert!(p.layer_name("udp").is_some()),
            _ => panic!("invalid Output type"),
        }

        match rtshark.read().unwrap() {
            Output::EOF => (),
            _ => panic!("invalid Output type"),
        }

        // stop tshark
        rtshark.kill();

        // reading from process output should give EOF
        match rtshark.read().unwrap() {
            Output::EOF => (),
            _ => panic!("invalid Output type"),
        }

        /* remove fifo & tempdir */
        tmp_dir.close().expect("Error deleting fifo dir");
    }

    #[test]
    fn test_rtshark_file_missing() {
        // start tshark on a missing fifo
        let builder = RTSharkBuilder::builder()
            .input_path("/missing/rtshark/fifo".to_string())
            .input_type(InputType::File)
            .build();

        let ret = builder.run();

        match ret {
            Ok(_) => panic!("We can't start if fifo is missing"),
            Err(e) => println!("{e}"),
        }
    }

    #[test]
    fn test_rtshark_fifo_missing() {
        // start tshark on a missing fifo
        let builder = RTSharkBuilder::builder()
            .input_path("/missing/rtshark/fifo".to_string())
            .input_type(InputType::Fifo)
            .build();

        let ret = builder.run();

        match ret {
            Ok(_) => panic!("We can't start if fifo is missing"),
            Err(e) => println!("{e}"),
        }
    }

    #[test]
    fn test_rtshark_tshark_missing() {
        // start tshark on a missing fifo
        let builder = RTSharkBuilder::builder()
            .input_path("/missing/rtshark/fifo".to_string())
            .input_type(InputType::Fifo)
            .env_path("/invalid/path")
            .build();

        let ret = builder.run();

        match ret {
            Ok(_) => panic!("We can't start if tshark is missing"),
            Err(e) => println!("{e}"),
        }
    }
}
