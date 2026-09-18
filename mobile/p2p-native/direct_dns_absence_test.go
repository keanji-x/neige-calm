package main

import (
	"testing"

	"golang.org/x/net/dns/dnsmessage"
)

func absencePacket() dnsmessage.Message {
	name := dnsmessage.MustNewName("example.com.")
	return dnsmessage.Message{
		Header:      dnsmessage.Header{Response: true, RecursionAvailable: true},
		Questions:   []dnsmessage.Question{{Name: name, Type: dnsmessage.TypeAAAA, Class: dnsmessage.ClassINET}},
		Authorities: []dnsmessage.Resource{{Header: dnsmessage.ResourceHeader{Name: name, Type: dnsmessage.TypeSOA, Class: dnsmessage.ClassINET}, Body: &dnsmessage.SOAResource{NS: name, MBox: name}}},
	}
}

func TestDirectDNSAbsenceRequiresCompleteNegativeResponse(t *testing.T) {
	for _, test := range []struct {
		name   string
		change func(*dnsmessage.Message)
		pass   bool
	}{
		{"no-data", func(*dnsmessage.Message) {}, true},
		{"servfail", func(m *dnsmessage.Message) { m.RCode = dnsmessage.RCodeServerFailure }, false},
		{"nxdomain", func(m *dnsmessage.Message) { m.RCode = dnsmessage.RCodeNameError }, false},
		{"truncated", func(m *dnsmessage.Message) { m.Truncated = true }, false},
		{"request-not-response", func(m *dnsmessage.Message) { m.Response = false }, false},
		{"wrong-name", func(m *dnsmessage.Message) { m.Questions[0].Name = dnsmessage.MustNewName("other.example.") }, false},
		{"wrong-family", func(m *dnsmessage.Message) { m.Questions[0].Type = dnsmessage.TypeA }, false},
		{"no-soa", func(m *dnsmessage.Message) { m.Authorities = nil }, false},
		{"unrelated-soa", func(m *dnsmessage.Message) { m.Authorities[0].Header.Name = dnsmessage.MustNewName("other.com.") }, false},
		{"positive-answer", func(m *dnsmessage.Message) {
			m.Answers = []dnsmessage.Resource{{Header: dnsmessage.ResourceHeader{Name: m.Questions[0].Name, Type: dnsmessage.TypeAAAA, Class: dnsmessage.ClassINET}, Body: &dnsmessage.AAAAResource{AAAA: [16]byte{0xfd, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8}}}}
		}, false},
		{"cname-negative", func(m *dnsmessage.Message) {
			m.Answers = []dnsmessage.Resource{{Header: dnsmessage.ResourceHeader{Name: m.Questions[0].Name, Type: dnsmessage.TypeCNAME, Class: dnsmessage.ClassINET}, Body: &dnsmessage.CNAMEResource{CNAME: dnsmessage.MustNewName("alias.example.com.")}}}
		}, true},
		{"cname-loop", func(m *dnsmessage.Message) {
			m.Answers = []dnsmessage.Resource{{Header: dnsmessage.ResourceHeader{Name: m.Questions[0].Name, Type: dnsmessage.TypeCNAME, Class: dnsmessage.ClassINET}, Body: &dnsmessage.CNAMEResource{CNAME: m.Questions[0].Name}}}
		}, false},
	} {
		t.Run(test.name, func(t *testing.T) {
			message := absencePacket()
			test.change(&message)
			packet, err := message.Pack()
			if err != nil {
				t.Fatal(err)
			}
			err = validateDirectDNSAbsence("example.com", dnsmessage.TypeAAAA, 0, packet)
			if (err == nil) != test.pass {
				t.Fatalf("absence accepted=%v, want=%v: %v", err == nil, test.pass, err)
			}
		})
	}
	t.Run("rcode-disagrees", func(t *testing.T) {
		m := absencePacket()
		packet, _ := m.Pack()
		if validateDirectDNSAbsence("example.com", dnsmessage.TypeAAAA, 2, packet) == nil {
			t.Fatal("ignored native rcode")
		}
	})
	t.Run("malformed-packet", func(t *testing.T) {
		if validateDirectDNSAbsence("example.com", dnsmessage.TypeAAAA, 0, []byte{0, 1}) == nil {
			t.Fatal("accepted malformed packet")
		}
	})
}
