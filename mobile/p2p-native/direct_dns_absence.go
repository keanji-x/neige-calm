package main

import (
	"errors"
	"strings"

	"golang.org/x/net/dns/dnsmessage"
)

func validateDirectDNSAbsence(host string, typeID dnsmessage.Type, rcode int, wire []byte) error {
	failure := errors.New("DNS absence is unproven; retry, use a literal IP or Tailnet")
	var response dnsmessage.Message
	if response.Unpack(wire) != nil || !response.Response || response.Truncated || response.OpCode != 0 ||
		rcode != 0 || response.RCode != dnsmessage.RCodeSuccess || len(response.Questions) != 1 {
		return failure
	}
	question := response.Questions[0]
	if question.Class != dnsmessage.ClassINET || question.Type != typeID ||
		!strings.EqualFold(question.Name.String(), strings.TrimSuffix(host, ".")+".") {
		return failure
	}
	// Require a complete CNAME chain (if present), ending in an SOA-backed
	// negative answer. Referrals, bare empty packets and renewed positive
	// answers cannot prove absence. Packet decoding is the pinned DNS library.
	name := strings.ToLower(question.Name.String())
	seen := map[string]bool{name: true}
	remaining := append([]dnsmessage.Resource(nil), response.Answers...)
	for len(remaining) > 0 {
		found := -1
		for i, resource := range remaining {
			if resource.Header.Class != dnsmessage.ClassINET || resource.Header.Type != dnsmessage.TypeCNAME {
				return failure
			}
			if strings.EqualFold(resource.Header.Name.String(), name) {
				if found != -1 {
					return failure
				}
				found = i
			}
		}
		if found == -1 {
			return failure
		}
		cname, ok := remaining[found].Body.(*dnsmessage.CNAMEResource)
		if !ok {
			return failure
		}
		name = strings.ToLower(cname.CNAME.String())
		if seen[name] {
			return failure
		}
		seen[name] = true
		remaining = append(remaining[:found], remaining[found+1:]...)
	}
	for _, authority := range response.Authorities {
		zone := strings.ToLower(authority.Header.Name.String())
		if authority.Header.Class == dnsmessage.ClassINET && authority.Header.Type == dnsmessage.TypeSOA &&
			(name == zone || strings.HasSuffix(name, "."+zone) || zone == ".") {
			if _, ok := authority.Body.(*dnsmessage.SOAResource); ok {
				return nil
			}
		}
	}
	return failure
}
